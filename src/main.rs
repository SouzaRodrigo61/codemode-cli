mod bench;
mod denylist;
mod gain;
mod preflight;
mod maestri;
mod primitives;
mod sandbox;
mod telemetry;

use clap::{Parser, Subcommand};
use rhai::Engine;
use sandbox::Sandbox;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "codemode", about = "Run a Rhai script as one sandboxed batch of file/shell primitives instead of N separate tool-calls.")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a .rhai script.
    Run {
        /// Path to the script, or "-" to read it from stdin.
        script: String,
        /// Directory the script is confined to. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
        /// Wall-clock timeout in seconds. Hard cap: 120s.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Max bytes of consolidated output before truncation.
        #[arg(long = "max-output", default_value_t = 1_048_576)]
        max_output: usize,
        /// Print a call log (each primitive invocation) to stderr for debugging.
        #[arg(long)]
        verbose: bool,
        /// Announce every mutating primitive instead of performing it: no
        /// write, no edit, no shell command. Reads still run.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Host allowed for `http_get` (repeatable). Default-closed: with no
        /// --allow-host, every http_get is refused. An entry "h" allows any
        /// port on h; "h:p" allows exactly that port. No wildcards.
        #[arg(long = "allow-host")]
        allow_host: Vec<String>,
        /// Positional argument passed to the script (repeatable, in order).
        /// Available inside the script as the constant array `ARGS` -- what
        /// makes a `.codemode/` library script reusable (`codemode run
        /// review-pr.rhai --arg 77`) instead of edited per invocation.
        #[arg(long = "arg")]
        script_args: Vec<String>,
    },
    /// Validate a script without running it: compile, resolve every called
    /// symbol against what is actually registered, and lint. Exits non-zero
    /// on the first problem -- meant for CI over a repo's `.codemode/`.
    Check {
        /// Path to the script, or "-" to read it from stdin.
        script: String,
        /// Directory the script is confined to (also where `.codemode/` is
        /// looked up). Defaults to the current directory.
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
    },
    /// Report what the recorded runs actually saved: tool-calls avoided,
    /// error rate, and where the waste is. Reads `~/.codemode/runs.jsonl`.
    Gain {
        /// List the most recent runs before the summary.
        #[arg(long)]
        history: bool,
        /// Emit the aggregate as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// How many runs `--history` lists.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Time a .rhai script's real wall-clock cost, natively -- no Python/shell
    /// timing harness, no interpreter-spawn overhead skewing the result.
    Bench {
        /// Path to the .rhai script to time (run via `codemode run`).
        script: String,
        /// Directory the script is confined to. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
        /// A shell command to run and time the same way, for a side-by-side
        /// comparison (e.g. the native tool-call equivalent of the script).
        #[arg(long)]
        compare: Option<String>,
        /// Shell command run before each iteration (both codemode and
        /// --compare), not counted in the timing -- e.g. `git checkout --
        /// fixtures/` to reset mutated fixtures between runs.
        #[arg(long = "reset-cmd")]
        reset_cmd: Option<String>,
        /// Number of iterations per side.
        #[arg(long, default_value_t = 30)]
        n: usize,
    },
}

const HARD_TIMEOUT_CAP: u64 = 120;

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check { script, workdir } => match check(&script, &workdir) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("codemode: {e}");
                std::process::exit(1);
            }
        },
        Commands::Run { script, workdir, timeout, max_output, verbose, dry_run, allow_host, script_args } => {
            let timeout = timeout.clamp(1, HARD_TIMEOUT_CAP);
            match run(&script, &workdir, timeout, max_output, verbose, dry_run, allow_host, script_args) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("codemode: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Gain { history, json, limit } => {
            match gain::run(gain::GainArgs { history, json, limit }) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("codemode: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Bench { script, workdir, compare, reset_cmd, n } => {
            let args = bench::BenchArgs { script, workdir, compare, reset_cmd, n };
            match bench::run(args) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("codemode: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

fn read_script(script: &str, workdir: &Path) -> Result<(String, &'static str), String> {
    if script == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("failed to read script from stdin: {e}"))?;
        return Ok((buf, "stdin"));
    }
    match std::fs::read_to_string(script) {
        Ok(s) => Ok((s, "file")),
        // Repo script library convention (issue #9): a bare name that
        // doesn't resolve as given also gets looked up in the workdir's
        // `.codemode/` directory, so a versioned library of reusable
        // scripts is directly runnable (`codemode run review.rhai`)
        // without each caller re-deriving the path -- the field-reported
        // failure mode is agents rewriting duplicate scripts every
        // session instead of reusing what the repo already has.
        Err(first_err) => {
            let is_bare_name = !script.contains('/') && !script.contains('\\');
            if is_bare_name {
                let lib_path = workdir.join(".codemode").join(script);
                if let Ok(s) = std::fs::read_to_string(&lib_path) {
                    return Ok((s, "lib"));
                }
            }
            Err(format!(
                "failed to read script {script:?}: {first_err}{}",
                if is_bare_name { " (also not found in <workdir>/.codemode/)" } else { "" }
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(script_arg: &str, workdir: &PathBuf, timeout_secs: u64, max_output: usize, verbose: bool, dry_run: bool, allow_hosts: Vec<String>, script_args: Vec<String>) -> Result<i32, String> {
    let started = Instant::now();
    let (source, origem) = read_script(script_arg, workdir)?;
    let counter = primitives::new_counter();
    let meta = RunMeta {
        script: telemetry::hash(&source),
        source: origem.to_string(),
        name: if origem == "stdin" { None } else { Some(script_arg.to_string()) },
        workdir: std::fs::canonicalize(workdir)
            .unwrap_or_else(|_| workdir.clone())
            .display()
            .to_string(),
    };

    let sandbox = Sandbox::new(workdir)?.with_dry(dry_run);

    let mut engine = Engine::new();
    primitives::register(&mut engine, sandbox, allow_hosts, counter.clone());
    maestri::register(&mut engine);
    let sink = primitives::register_output_capture(&mut engine, max_output);

    // Pré-voo: compila, resolve símbolo e linta ANTES da primeira primitiva
    // (#13/#15/#17). Um `Function not found` na linha 8 costumava aparecer
    // depois de cinco run_shell já terem rodado.
    let relatorio = match preflight::check(&engine, &source) {
        Ok(r) => r,
        Err(erros) => {
            for e in &erros {
                eprintln!("codemode: {e}");
            }
            record_run(&meta, &counter, &sink, 1, started);
            return Ok(1);
        }
    };
    for w in &relatorio.warnings {
        eprintln!("codemode: dica: {w}");
    }
    if dry_run {
        eprintln!("codemode: --dry-run: nada será escrito nem executado");
    }

    if verbose {
        eprintln!("codemode: workdir={:?} timeout={}s max_output={}B", std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.clone()), timeout_secs, max_output);
        match detect_host_sandbox() {
            Some(v) => eprintln!("codemode: host sandbox signal detected: {v} (informational only, best-effort — codemode's own confinement/denylist run unconditionally either way)"),
            None => eprintln!("codemode: no known host sandbox env var detected (informational only, best-effort — absence doesn't mean no sandbox; codemode's own confinement/denylist run unconditionally either way)"),
        }
    }

    // Primary defense against pure-VM infinite loops (e.g. `loop {}`):
    // Rhai calls this progress hook roughly once per VM operation, so we
    // can abort cleanly by returning Some(..) once the deadline passes.
    let start = Instant::now();
    let deadline = Duration::from_secs(timeout_secs);
    engine.on_progress(move |_ops| {
        if start.elapsed() >= deadline {
            Some(rhai::Dynamic::from("codemode: script exceeded timeout".to_string()))
        } else {
            None
        }
    });

    // Backup watchdog: on_progress only fires between VM operations, so it
    // cannot interrupt a script blocked *inside* a native call (e.g.
    // run_shell spawning `sleep 999`). Rust has no safe way to kill a
    // thread mid-execution, so as a last resort we let the whole process
    // die when the hard deadline passes — this takes the stuck native call
    // down with it. This is a known, documented limitation, not an
    // oversight.
    let (tx, rx) = mpsc::channel();
    let ast = relatorio.ast;
    let handle = std::thread::spawn(move || {
        // ARGS is a scope CONSTANT (not a global var) so a script can't
        // shadow-assign it by accident and then read stale values.
        let args_array: rhai::Array =
            script_args.into_iter().map(rhai::Dynamic::from).collect();
        let mut scope = rhai::Scope::new();
        scope.push_constant("ARGS", args_array);
        let result = engine.eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast);
        let _ = tx.send(result);
    });

    match rx.recv_timeout(deadline + Duration::from_secs(1)) {
        Ok(Ok(value)) => {
            if !value.is_unit() {
                let mut s = sink.lock().unwrap();
                let as_str = value.to_string();
                s.push(&as_str);
                s.push("\n");
            }
        }
        Ok(Err(e)) => {
            if let rhai::EvalAltResult::ErrorTerminated(token, _) = &*e {
                // Two things terminate a script uncatchably: the timeout
                // watchdog, and a denylist refusal (which must not be
                // swallowable by the script's own try/catch -- see
                // run_shell_impl). Tell them apart by the token.
                let token = token.to_string();
                if let Some(msg) = token.strip_prefix("denylist:") {
                    eprintln!("codemode: {msg}");
                    print_sink(&sink);
                    record_run(&meta, &counter, &sink, 1, started);
                    return Ok(1);
                }
                eprintln!("codemode: script exceeded {timeout_secs}s timeout, aborted");
                record_run(&meta, &counter, &sink, 124, started);
                return Ok(124);
            }
            eprintln!("codemode: script error: {e}");
            if let Some(t) = preflight::excerpt(&source, e.position()) {
                eprint!("{t}");
            }
            for hint in preflight::foreign_idiom_hints(&source) {
                eprintln!("codemode: dica: {hint}");
            }
            print_sink(&sink);
            record_run(&meta, &counter, &sink, 1, started);
            return Ok(1);
        }
        Err(_) => {
            // Watchdog fallback: still not done past the hard deadline.
            // The eval thread may be stuck in a blocking native call; we
            // cannot safely join/kill it, so exit the whole process.
            eprintln!("codemode: script exceeded {timeout_secs}s timeout (watchdog), aborting process");
            record_run(&meta, &counter, &sink, 124, started);
            std::process::exit(124);
        }
    }

    let _ = handle.join();
    print_sink(&sink);
    record_run(&meta, &counter, &sink, 0, started);
    Ok(0)
}

/// `codemode check`: pré-voo sem execução. Existe para o CI de um repo
/// poder validar a própria biblioteca `.codemode/` (#16).
fn check(script_arg: &str, workdir: &Path) -> Result<i32, String> {
    let (source, _origem) = read_script(script_arg, workdir)?;
    let sandbox = Sandbox::new(workdir)?;
    let mut engine = Engine::new();
    primitives::register(&mut engine, sandbox, Vec::new(), primitives::new_counter());
    maestri::register(&mut engine);
    match preflight::check(&engine, &source) {
        Ok(r) => {
            for w in &r.warnings {
                eprintln!("codemode: dica: {w}");
            }
            println!(
                "ok: {script_arg} compila, {} primitiva(s) referenciada(s){}",
                r.prim_calls,
                if r.has_loop { ", com laço" } else { "" }
            );
            Ok(0)
        }
        Err(erros) => {
            for e in &erros {
                eprintln!("codemode: {e}");
            }
            Ok(1)
        }
    }
}

/// O que a telemetria sabe da execução antes dela terminar.
struct RunMeta {
    script: String,
    source: String,
    name: Option<String>,
    workdir: String,
}

/// Grava a linha de telemetria. Chamada em TODA saída de `run` -- inclusive
/// nas de falha, porque a taxa de erro é justamente um dos números que o
/// relatório existe para expor (issue #11/#12).
fn record_run(
    meta: &RunMeta,
    counter: &primitives::Counter,
    sink: &primitives::SharedSink,
    exit_code: i32,
    started: Instant,
) {
    let prims: std::collections::BTreeMap<String, u64> =
        counter.lock().map(|m| m.clone()).unwrap_or_default();
    let prim_total = prims.values().sum();
    // Bytes que de fato chegam ao contexto do chamador -- o buffer impresso,
    // não o spill em disco, que existe justamente para NÃO ser lido.
    let out_bytes = sink.lock().map(|s| s.buf.len() as u64).unwrap_or(0);
    telemetry::record(&telemetry::Entry {
        ts: telemetry::now_secs(),
        script: meta.script.clone(),
        source: meta.source.clone(),
        name: meta.name.clone(),
        prims,
        prim_total,
        out_bytes,
        exit_code,
        ms: started.elapsed().as_millis() as u64,
        workdir: meta.workdir.clone(),
    });
}

/// Best-effort, informational only: checks a few plausible env var names
/// a host CLI *might* set to signal it's already running inside an OS
/// sandbox (Seatbelt / bubblewrap / Landlock). None of these are
/// documented/confirmed by the tools themselves as of this writing — on
/// this machine, none of them were present in the environment. Absence
/// is not evidence of absence. This value is never used to relax
/// codemode's own confinement or denylist; those always run regardless.
fn detect_host_sandbox() -> Option<String> {
    for var in ["CLAUDE_SANDBOX", "CODEX_SANDBOX", "SANDBOX", "IS_SANDBOX"] {
        if let Ok(v) = std::env::var(var) {
            return Some(format!("{var}={v}"));
        }
    }
    None
}

fn print_sink(sink: &primitives::SharedSink) {
    let s = sink.lock().unwrap();
    print!("{}", s.buf);
    if s.truncated {
        match &s.spill_path {
            Some(path) => {
                eprintln!(
                    "codemode: output truncated at cap ({} bytes); full output: {}",
                    s.cap,
                    path.display()
                );
                if let Some(tail) = s.tail_preview(512) {
                    let tail = tail.trim_end();
                    if !tail.is_empty() {
                        eprintln!("codemode: tail of full output:\n{tail}");
                    }
                }
            }
            None => eprintln!(
                "codemode: output truncated at cap ({} bytes); spill unavailable, overflow lost",
                s.cap
            ),
        }
    }
}
