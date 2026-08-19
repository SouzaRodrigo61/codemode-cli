mod bench;
mod denylist;
mod maestri;
mod primitives;
mod sandbox;

use clap::{Parser, Subcommand};
use rhai::Engine;
use sandbox::Sandbox;
use std::io::Read;
use std::path::PathBuf;
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
        Commands::Run { script, workdir, timeout, max_output, verbose, allow_host, script_args } => {
            let timeout = timeout.min(HARD_TIMEOUT_CAP).max(1);
            match run(&script, &workdir, timeout, max_output, verbose, allow_host, script_args) {
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

fn read_script(script: &str, workdir: &PathBuf) -> Result<String, String> {
    if script == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("failed to read script from stdin: {e}"))?;
        return Ok(buf);
    }
    match std::fs::read_to_string(script) {
        Ok(s) => Ok(s),
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
                    return Ok(s);
                }
            }
            Err(format!(
                "failed to read script {script:?}: {first_err}{}",
                if is_bare_name { " (also not found in <workdir>/.codemode/)" } else { "" }
            ))
        }
    }
}

fn run(script_arg: &str, workdir: &PathBuf, timeout_secs: u64, max_output: usize, verbose: bool, allow_hosts: Vec<String>, script_args: Vec<String>) -> Result<i32, String> {
    let source = read_script(script_arg, workdir)?;
    let sandbox = Sandbox::new(workdir)?;

    let mut engine = Engine::new();
    primitives::register(&mut engine, sandbox, allow_hosts);
    maestri::register(&mut engine);
    let sink = primitives::register_output_capture(&mut engine, max_output);

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
    let ast_source = source.clone();
    let handle = std::thread::spawn(move || {
        // ARGS is a scope CONSTANT (not a global var) so a script can't
        // shadow-assign it by accident and then read stale values.
        let args_array: rhai::Array =
            script_args.into_iter().map(rhai::Dynamic::from).collect();
        let mut scope = rhai::Scope::new();
        scope.push_constant("ARGS", args_array);
        let result = engine.eval_with_scope::<rhai::Dynamic>(&mut scope, &ast_source);
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
                    return Ok(1);
                }
                eprintln!("codemode: script exceeded {timeout_secs}s timeout, aborted");
                return Ok(124);
            }
            eprintln!("codemode: script error: {e}");
            for hint in foreign_idiom_hints(&source) {
                eprintln!("codemode: dica: {hint}");
            }
            print_sink(&sink);
            return Ok(1);
        }
        Err(_) => {
            // Watchdog fallback: still not done past the hard deadline.
            // The eval thread may be stuck in a blocking native call; we
            // cannot safely join/kill it, so exit the whole process.
            eprintln!("codemode: script exceeded {timeout_secs}s timeout (watchdog), aborting process");
            std::process::exit(124);
        }
    }

    let _ = handle.join();
    print_sink(&sink);
    Ok(0)
}

/// Best-effort, informational only: checks a few plausible env var names
/// a host CLI *might* set to signal it's already running inside an OS
/// sandbox (Seatbelt / bubblewrap / Landlock). None of these are
/// documented/confirmed by the tools themselves as of this writing — on
/// this machine, none of them were present in the environment. Absence
/// is not evidence of absence. This value is never used to relax
/// codemode's own confinement or denylist; those always run regardless.
/// Targeted hints appended to a script error when the source shows an
/// idiom from another language. Models writing Rhai (a niche language)
/// reach for JS/Rust muscle memory; a bare "Variable not found: console"
/// costs the caller a whole extra LLM round-trip to re-diagnose, while one
/// hint line usually fixes it on the first retry. Only fires on failure --
/// zero cost on the success path -- and only for idioms actually present.
fn foreign_idiom_hints(source: &str) -> Vec<&'static str> {
    const HINTS: &[(&str, &str)] = &[
        ("console.", "Rhai não tem console — use print(x)"),
        ("println!", "Rhai não é Rust — use print(x), sem macros"),
        ("format!", "Rhai não tem format! — use interpolação `texto ${x}` ou concatenação +"),
        ("function ", "funções em Rhai usam fn nome() { }, não function"),
        ("=>", "closure em Rhai é |x| expr, não arrow function =>"),
        ("===", "Rhai usa == e !=, não === / !=="),
        ("require(", "Rhai não tem require/import — só as funções nativas do codemode (read_file, run_shell, ...)"),
        ("import ", "Rhai não tem import — só as funções nativas do codemode"),
        ("JSON.", "Rhai não tem objeto JSON — trate o texto com as funções de string ou run_shell"),
        (".forEach", "Rhai não tem forEach — use for x in lista { }"),
        ("let mut ", "Rhai não usa mut — toda variável let já é mutável"),
    ];
    HINTS
        .iter()
        .filter(|(pat, _)| source.contains(pat))
        .map(|(_, hint)| *hint)
        .take(3)
        .collect()
}

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
