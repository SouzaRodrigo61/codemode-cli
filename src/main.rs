mod bench;
mod denylist;
mod biblioteca;
mod gain;
mod preflight;
mod maestri;
mod primitives;
mod sandbox;
mod telemetry;

use clap::{Parser, Subcommand};
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
        /// Wall-clock timeout for the whole script, in seconds. 0 disables
        /// it -- the caller takes responsibility. A pure VM loop is still
        /// caught by --vm-idle, which is independent of this.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Seconds a single shell command may run before being killed.
        /// 0 disables. This is what makes `cargo test` inside a script
        /// possible without the whole run hanging forever.
        #[arg(long = "cmd-timeout", default_value_t = 600)]
        cmd_timeout: u64,
        /// Abort if this many seconds pass without a single primitive being
        /// dispatched -- the guard against `loop {}`, independent of how
        /// long the script as a whole is allowed to take.
        #[arg(long = "vm-idle", default_value_t = 30)]
        vm_idle: u64,
        /// Additional directory the script may read and write (repeatable).
        /// Each root is confined on its own; there is no path between them.
        #[arg(long = "extra-root")]
        extra_root: Vec<PathBuf>,
        /// Max bytes of consolidated output before truncation. This is the
        /// runaway-script guard, not a context budget -- see --max-context.
        #[arg(long = "max-output", default_value_t = 1_048_576)]
        max_output: usize,
        /// Warn (never truncate) when the script's output passes this many
        /// bytes. 64 KiB is roughly 16k tokens: past that, a script that
        /// collapsed ten tool-calls may still be a net loss.
        #[arg(long = "max-context", default_value_t = 65_536)]
        max_context: usize,
        /// Print a call log (each primitive invocation) to stderr for debugging.
        #[arg(long)]
        verbose: bool,
        /// Refuse to run a script that collapses fewer than two primitives:
        /// wrapping a single call in Rhai costs more than the plain Bash
        /// call it replaces.
        #[arg(long)]
        strict: bool,
        /// Emit one JSON object (output, exit code, primitive counts,
        /// duration) instead of the raw script output.
        #[arg(long)]
        json: bool,
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
    /// Copy the last script you ran (or --from) into `<workdir>/.codemode/`
    /// so it stops being scratchpad litter and starts being a repo asset.
    Save {
        /// Name in the library; `.rhai` is appended if missing.
        nome: String,
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
        /// Source to save: a path, or "-" for stdin. Defaults to the last
        /// script this machine ran.
        #[arg(long)]
        from: Option<String>,
        /// One-line description, written as the `// desc:` header.
        #[arg(long)]
        desc: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// List this repo's `.codemode/` library: description, whether it takes
    /// `--arg`, and how many times each script has actually run.
    List {
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
    },
    /// Print the Rhai idioms and traps that cost the most wasted runs.
    Idioms,
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
        /// Report the EXCLUDED segment instead -- benchmark cases, scratch
        /// scripts in a temp dir, and codemode developing itself. The
        /// default report covers real work only.
        #[arg(long)]
        bench: bool,
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


fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Save { nome, workdir, from, desc, force } => {
            match biblioteca::save(biblioteca::SaveArgs { nome, workdir, from, desc, force }) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("codemode: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::List { workdir } => match biblioteca::list(&workdir) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("codemode: {e}");
                std::process::exit(1);
            }
        },
        Commands::Idioms => std::process::exit(biblioteca::idioms()),
        Commands::Check { script, workdir } => match check(&script, &workdir) {
            Ok(code) => std::process::exit(code),
            Err(e) => {
                eprintln!("codemode: {e}");
                std::process::exit(1);
            }
        },
        Commands::Run {
            script,
            workdir,
            timeout,
            cmd_timeout,
            vm_idle,
            extra_root,
            max_output,
            max_context,
            verbose,
            strict,
            json,
            dry_run,
            allow_host,
            script_args,
        } => {
            let opts = RunOpts { timeout, cmd_timeout, vm_idle, extra_root, max_output, max_context, verbose, strict, json, dry_run, allow_hosts: allow_host };
            match run(&script, &workdir, opts, script_args) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("codemode: {e}");
                    std::process::exit(1);
                }
            }
        }
        Commands::Gain { history, json, limit, bench } => {
            match gain::run(gain::GainArgs { history, json, limit, bench }) {
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

/// Opções de uma execução. Viraram struct quando `run` passou de 8
/// parâmetros -- e porque #18 acrescentou três de uma vez.
struct RunOpts {
    timeout: u64,
    cmd_timeout: u64,
    vm_idle: u64,
    extra_root: Vec<PathBuf>,
    max_output: usize,
    max_context: usize,
    verbose: bool,
    strict: bool,
    json: bool,
    dry_run: bool,
    allow_hosts: Vec<String>,
}

fn run(script_arg: &str, workdir: &Path, opts: RunOpts, script_args: Vec<String>) -> Result<i32, String> {
    let RunOpts { timeout: timeout_secs, cmd_timeout, vm_idle, extra_root, max_output, max_context, verbose, strict, json, dry_run, allow_hosts } = opts;
    let started = Instant::now();
    let (source, origem) = read_script(script_arg, workdir)?;
    let counter = primitives::new_counter();
    let meta = RunMeta {
        script: telemetry::hash(&source),
        source: origem.to_string(),
        name: if origem == "stdin" { None } else { Some(script_arg.to_string()) },
        workdir: std::fs::canonicalize(workdir)
            .unwrap_or_else(|_| workdir.to_path_buf())
            .display()
            .to_string(),
        kind: String::new(),
    };
    // `--dry-run` não executa nada: contá-la como trabalho real inflaria o
    // relatório com execução que nunca tocou em arquivo nenhum.
    let meta = RunMeta {
        kind: if dry_run {
            "check".to_string()
        } else {
            telemetry::classify(&meta.workdir, meta.name.as_deref())
        },
        ..meta
    };

    let sandbox = Sandbox::new(workdir)?
        .with_dry(dry_run)
        .with_cmd_timeout(cmd_timeout)
        .with_extra_roots(&extra_root)?;

    let mut engine = primitives::nova_engine();
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
    // Guarda de trivialidade (#21): 34 das 200 execuções auditadas
    // embrulharam UMA primitiva em Rhai -- custo líquido negativo. Script
    // com laço fica de fora: uma chamada no fonte pode ser N em execução.
    if relatorio.prim_calls < 2 && !relatorio.has_loop {
        let equivalente = primitiva_unica_como_shell(&source)
            .map(|c| format!(" -- o equivalente direto é: {c}"))
            .unwrap_or_default();
        eprintln!(
            "codemode: aviso: {} primitiva(s) neste script: Bash direto sai mais barato{}",
            relatorio.prim_calls, equivalente
        );
        if strict {
            eprintln!("codemode: --strict: recusado sem executar");
            record_run(&meta, &counter, &sink, 2, started);
            return Ok(2);
        }
    }
    // O `save` precisa do fonte do último script -- mas só quando ele veio
    // de stdin. Script que veio de arquivo já ESTÁ em disco: gravar uma
    // cópia custava 0,28ms em toda execução para nada (#42), e o `save
    // --from <caminho>` cobre esse caso.
    if origem == "stdin" {
        guarda_ultimo_script(&source);
    }
    if dry_run {
        eprintln!("codemode: --dry-run: nada será escrito nem executado");
    }

    if verbose {
        eprintln!("codemode: workdir={:?} timeout={}s max_output={}B", std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf()), timeout_secs, max_output);
        match detect_host_sandbox() {
            Some(v) => eprintln!("codemode: host sandbox signal detected: {v} (informational only, best-effort — codemode's own confinement/denylist run unconditionally either way)"),
            None => eprintln!("codemode: no known host sandbox env var detected (informational only, best-effort — absence doesn't mean no sandbox; codemode's own confinement/denylist run unconditionally either way)"),
        }
    }

    // Duas guardas independentes, porque são dois problemas diferentes
    // (#18):
    //
    // 1. Deadline global (`--timeout`, 0 = desligado): o script como um
    //    todo. Deixou de ter cap de 120s -- um script que edita, roda a
    //    suíte e decide pelo resultado precisa de minutos, e proibir isso
    //    era o que quebrava todo fluxo de verificação em duas tool-calls.
    // 2. Ociosidade de VM (`--vm-idle`): tempo sem NENHUMA primitiva
    //    despachada. É o que pega `loop {}`, e continua valendo mesmo com
    //    --timeout 0, porque laço puro não chama primitiva nenhuma. O sinal
    //    vem do contador de telemetria, que já existe.
    let start = Instant::now();
    let deadline = (timeout_secs > 0).then(|| Duration::from_secs(timeout_secs));
    let ociosidade = Duration::from_secs(vm_idle.max(1));
    let ultimo_total = std::sync::atomic::AtomicU64::new(0);
    let ultimo_ms = std::sync::atomic::AtomicU64::new(0);
    engine.on_progress(move |ops| {
        use std::sync::atomic::Ordering;
        // Amostragem: este hook roda a cada operacao de VM, e duas leituras
        // de relogio por operacao apareciam em 27% do tempo de um script
        // computacional (#39). A cada 1024 operacoes ainda e granularidade
        // de microssegundos -- muito mais fina que os segundos que as duas
        // guardas medem.
        if ops % 1024 != 0 {
            return None;
        }
        let agora = start.elapsed();
        let total: u64 = primitives::total_chamadas();
        if total != ultimo_total.load(Ordering::Relaxed) {
            ultimo_total.store(total, Ordering::Relaxed);
            ultimo_ms.store(agora.as_millis() as u64, Ordering::Relaxed);
        }
        if let Some(d) = deadline {
            if agora >= d {
                return Some(rhai::Dynamic::from("codemode: script exceeded timeout".to_string()));
            }
        }
        let parado = agora.saturating_sub(Duration::from_millis(ultimo_ms.load(Ordering::Relaxed)));
        if parado >= ociosidade {
            return Some(rhai::Dynamic::from("codemode: script exceeded timeout".to_string()));
        }
        None
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

    let recebido = match deadline {
        Some(d) => rx.recv_timeout(d + Duration::from_secs(1)).map_err(|_| ()),
        // Sem deadline global o watchdog de processo não se aplica: quem
        // protege é a guarda de ociosidade, que aborta o script por dentro.
        None => rx.recv().map_err(|_| ()),
    };
    match recebido {
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
                    print_sink(&sink, max_context);
                    record_run(&meta, &counter, &sink, 1, started);
                    return Ok(1);
                }
                eprintln!(
                    "codemode: script abortado -- estourou o limite (timeout={timeout_secs}s, vm-idle={vm_idle}s)"
                );
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
            if json {
                imprime_json(&counter, &sink, 1, started, max_context);
            } else {
                print_sink(&sink, max_context);
            }
            record_run(&meta, &counter, &sink, 1, started);
            return Ok(1);
        }
        Err(()) => {
            // Watchdog fallback: still not done past the hard deadline.
            // The eval thread may be stuck in a blocking native call; we
            // cannot safely join/kill it, so exit the whole process.
            // Nomear o que estava em voo transforma um exit 124 mudo em
            // diagnóstico: sem isto, a execução inteira vira lixo e alguém
            // reescreve o script só para bissectar qual comando pendurou (#79).
            match primitives::em_voo() {
                Some(alvo) => eprintln!(
                    "codemode: script exceeded {timeout_secs}s timeout (watchdog) durante {alvo}, aborting process"
                ),
                None => eprintln!(
                    "codemode: script exceeded {timeout_secs}s timeout (watchdog), aborting process -- \
                     nenhuma chamada nativa em voo: provavelmente laço puro de VM (veja --vm-idle)"
                ),
            }
            record_run(&meta, &counter, &sink, 124, started);
            std::process::exit(124);
        }
    }

    let _ = handle.join();
    if json {
        imprime_json(&counter, &sink, 0, started, max_context);
    } else {
        print_sink(&sink, max_context);
    }
    record_run(&meta, &counter, &sink, 0, started);
    Ok(0)
}

/// Se o script tem uma única primitiva e ela é um `run_shell` com literal,
/// o aviso do #21 diz qual comando rodar direto.
fn primitiva_unica_como_shell(source: &str) -> Option<String> {
    let at = source.find("run_shell(\"")?;
    let resto = &source[at + "run_shell(\"".len()..];
    // Para na primeira aspa NÃO escapada. Parar na primeira aspa qualquer
    // cortava `run_shell("sh -c \"ls -la\"")` em `sh -c \` -- ou seja, o aviso
    // falhava exatamente quando o comando era não-trivial, que é quando a
    // sugestão importa (#80). A aspa escapada volta ao literal na sugestão:
    // quem vai colar no shell quer `sh -c "ls -la"`, não o escape do Rhai.
    let mut saida = String::new();
    let mut escapado = false;
    for c in resto.chars() {
        if escapado {
            // Só a aspa e a própria barra são escapes que interessam aqui; o
            // resto passa como veio, sem inventar interpretação.
            if c != '"' && c != '\\' {
                saida.push('\\');
            }
            saida.push(c);
            escapado = false;
            continue;
        }
        match c {
            '\\' => escapado = true,
            '"' => return Some(trunca_sugestao(saida)),
            _ => saida.push(c),
        }
    }
    None
}

/// A sugestão vai pro stderr, e stderr também é contexto. Um `run_shell` com
/// comando de 200 caracteres despejava tudo -- trocava o problema que o #80
/// resolveu por um do tipo que o #62 existe pra evitar. Mesmo limite do
/// watchdog (#79), pelo mesmo motivo: serve pra identificar, não pra colar de
/// olhos fechados.
fn trunca_sugestao(cmd: String) -> String {
    const LIMITE: usize = 70;
    if cmd.chars().count() <= LIMITE {
        return cmd;
    }
    let corte: String = cmd.chars().take(LIMITE - 3).collect();
    format!("{corte}...")
}

/// `--json` (#2): a saída vira dado, não prosa pro modelo reparsear.
fn imprime_json(
    counter: &primitives::Counter,
    sink: &primitives::SharedSink,
    exit_code: i32,
    started: Instant,
    max_context: usize,
) {
    let prims: std::collections::BTreeMap<String, u64> = counter.lock().map(|m| m.clone()).unwrap_or_default();
    let prim_total: u64 = prims.values().sum();
    let (saida, truncado, maior_push) =
        sink.lock().map(|s| (s.buf.clone(), s.truncated, s.maior_push)).unwrap_or_default();
    // O aviso de contexto (#62) tambem sai no stderr aqui: `--json` e o modo que
    // um agente usa, e era exatamente onde ele nao existia -- a guarda ficava
    // inerte justamente no caminho que ela existe para proteger.
    if let Ok(s) = sink.lock() {
        avisa_contexto(&s, max_context);
    }
    let corpo = serde_json::json!({
        "exit_code": exit_code,
        "output": saida,
        "truncated": truncado,
        // Byte de contexto e o que custa token; quem consome o JSON precisa do
        // numero para decidir, nao so de uma linha em stderr que pode se perder.
        "out_bytes": saida.len(),
        "max_context": max_context,
        "over_context": max_context > 0 && saida.len() > max_context,
        "largest_print": maior_push,
        "prims": prims,
        "prim_total": prim_total,
        "calls_avoided": prim_total.saturating_sub(1),
        "ms": started.elapsed().as_millis() as u64,
    });
    println!("{corpo}");
}

fn guarda_ultimo_script(source: &str) {
    if std::env::var("CODEMODE_NO_TELEMETRY").is_ok() {
        return;
    }
    if let Some(home) = telemetry::home() {
        if std::fs::create_dir_all(&home).is_ok() {
            let _ = std::fs::write(home.join("last.rhai"), source);
        }
    }
}

/// `codemode check`: pré-voo sem execução. Existe para o CI de um repo
/// poder validar a própria biblioteca `.codemode/` (#16).
fn check(script_arg: &str, workdir: &Path) -> Result<i32, String> {
    let (source, _origem) = read_script(script_arg, workdir)?;
    let sandbox = Sandbox::new(workdir)?;
    let mut engine = primitives::nova_engine();
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
    /// Classificado uma vez, na entrada: "real" | "bench" | "self" | "check".
    /// Gravar aqui, e não deduzir na leitura, é o que torna o relatório do
    /// #59 confiável -- a leitura só reclassifica linha antiga.
    kind: String,
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
        kind: Some(meta.kind.clone()),
    });
}

/// Best-effort, informational only: checks a few plausible env var names
/// a host CLI *might* set to signal it's already running inside an OS
/// sandbox (Seatbelt / bubblewrap / Landlock). None of these are
/// documented/confirmed by the tools themselves as of this writing — on
/// this machine, none of them were present in the environment. Absence
/// is not evidence of absence. This value is never used to relax
/// codemode's own confinement or denylist; those always run regardless.
/// Aviso de orcamento de CONTEXTO -- separado do `--max-output`, que e a
/// defesa contra script desgovernado.
///
/// O default do `--max-output` e 1 MiB, ~250 mil tokens: como guarda de
/// seguranca esta certo, como economia de contexto e inerte, porque nenhum
/// script real chega perto e a guarda nunca dispara (#62). A saida media
/// medida no uso real e 4,7 KB.
///
/// Aqui o limiar e de contexto e o efeito e AVISAR, nunca cortar: cortar
/// esconderia resultado, e o problema nao e a saida existir, e ninguem saber
/// que ela custou caro. Vai pro stderr de proposito -- stdout e a carga que
/// chega ao contexto, e sujar stdout seria trabalhar contra o proprio aviso.
fn avisa_contexto(s: &primitives::OutputSink, max_context: usize) {
    if max_context == 0 || s.buf.len() <= max_context {
        return;
    }
    eprintln!(
        "codemode: aviso: {} B de saida (limiar de contexto: {} B, ~{}k tokens) -- \
         um script que evita 10 tool-calls e despeja isso no contexto e prejuizo liquido",
        s.buf.len(),
        max_context,
        max_context / 4000
    );
    if s.maior_push > 0 {
        let fatia = s.maior_push * 100 / s.buf.len().max(1);
        eprintln!(
            "codemode: a maior impressao unica foi {} B ({}% do total) -- \
             comece por ela; read_file(caminho, #{{lines: \"i-j\"}}) costuma resolver",
            s.maior_push, fatia
        );
    }
}

fn detect_host_sandbox() -> Option<String> {
    for var in ["CLAUDE_SANDBOX", "CODEX_SANDBOX", "SANDBOX", "IS_SANDBOX"] {
        if let Ok(v) = std::env::var(var) {
            return Some(format!("{var}={v}"));
        }
    }
    None
}

fn print_sink(sink: &primitives::SharedSink, max_context: usize) {
    let s = sink.lock().unwrap();
    print!("{}", s.buf);
    avisa_contexto(&s, max_context);
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
