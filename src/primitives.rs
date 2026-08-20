//! Native Rust functions registered into the Rhai engine: the whole
//! point of code mode. A script calls these in sequence inside one
//! process instead of the caller issuing N separate tool-calls.

use crate::denylist;
use crate::sandbox::Sandbox;
use rhai::{Array, Engine, EvalAltResult, Map};
use std::fs;
use std::io::Read as _;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Captures everything the script prints (via `print`/`debug`) plus its
/// final return value, capped at `cap` bytes. Past the cap, nothing is
/// silently lost: the FULL stream (from byte zero) spills to a temp file
/// so the caller can recover what the cap hid -- truncation that discards
/// the overflow reads as "covered everything" when it didn't (the exact
/// output-ledger lesson from DeepSeek Harness: overflow must be an
/// explicit, recoverable condition, never a shorter string disguised as
/// the whole output). `std::env::temp_dir()` honors `$TMPDIR`, which in
/// the agent harnesses this runs under points at the session scratchpad.
pub struct OutputSink {
    pub buf: String,
    pub cap: usize,
    pub truncated: bool,
    pub spill_path: Option<std::path::PathBuf>,
    spill_file: Option<fs::File>,
}

impl OutputSink {
    pub fn new(cap: usize) -> Self {
        OutputSink { buf: String::new(), cap, truncated: false, spill_path: None, spill_file: None }
    }

    pub fn push(&mut self, s: &str) {
        if self.truncated {
            self.spill(s);
            return;
        }
        let remaining = self.cap.saturating_sub(self.buf.len());
        if s.len() <= remaining {
            self.buf.push_str(s);
        } else {
            // push what fits, on a char boundary
            let mut end = remaining.min(s.len());
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            self.buf.push_str(&s[..end]);
            self.truncated = true;
            // Seed the spill with the full stream so far (everything the
            // cap admitted, i.e. self.buf), then the part of this push the
            // cap rejected -- so the file is the byte-exact full output.
            self.start_spill();
            let seeded = self.buf.clone();
            self.spill(&seeded);
            self.spill(&s[end..]);
        }
    }

    fn start_spill(&mut self) {
        let path = std::env::temp_dir().join(format!("codemode-spill-{}.log", std::process::id()));
        match fs::File::create(&path) {
            Ok(f) => {
                self.spill_file = Some(f);
                self.spill_path = Some(path);
            }
            // Spill is best-effort: if the temp dir is unwritable we
            // degrade to plain truncation (the pre-spill behavior), and
            // spill_path stays None so the caller reports that honestly.
            Err(_) => {
                self.spill_file = None;
                self.spill_path = None;
            }
        }
    }

    fn spill(&mut self, s: &str) {
        use std::io::Write as _;
        if let Some(f) = self.spill_file.as_mut() {
            if f.write_all(s.as_bytes()).is_err() {
                self.spill_file = None;
                self.spill_path = None;
            }
        }
    }

    /// Last `max_bytes` of the spilled full output (line-aligned when
    /// possible) -- the tail preview shown next to the truncation notice,
    /// since the capped buffer already shows the head.
    pub fn tail_preview(&self, max_bytes: usize) -> Option<String> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let path = self.spill_path.as_ref()?;
        let mut f = fs::File::open(path).ok()?;
        let len = f.metadata().ok()?.len();
        let start = len.saturating_sub(max_bytes as u64);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf);
        // Drop the (likely partial) first line unless we read from byte 0.
        let text = if start > 0 {
            text.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or_else(|| text.into_owned())
        } else {
            text.into_owned()
        };
        Some(text)
    }
}

pub type SharedSink = Arc<Mutex<OutputSink>>;

pub fn register_output_capture(engine: &mut Engine, cap: usize) -> SharedSink {
    let sink: SharedSink = Arc::new(Mutex::new(OutputSink::new(cap)));
    let s1 = sink.clone();
    engine.on_print(move |s| {
        let mut sink = s1.lock().unwrap();
        sink.push(s);
        sink.push("\n");
    });
    let s2 = sink.clone();
    engine.on_debug(move |s, _src, _pos| {
        let mut sink = s2.lock().unwrap();
        sink.push(s);
        sink.push("\n");
    });
    sink
}

fn to_err(msg: impl Into<String>) -> Box<EvalAltResult> {
    msg.into().into()
}

// ---- read_file ----

fn read_file_impl(sandbox: &Sandbox, path: &str) -> Result<String, Box<EvalAltResult>> {
    let resolved = sandbox.resolve(path).map_err(to_err)?;
    // Sem `exists()` antes do open: era um `stat` a mais por leitura, e o
    // proprio open ja distingue "nao existe" de "sem permissao" (#43).
    let mut f = fs::File::open(&resolved).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            to_err(format!("read_file: {:?} does not exist", path))
        } else {
            to_err(format!("read_file: {:?}: {e}", path))
        }
    })?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| to_err(format!("read_file: {:?}: {e}", path)))?;
    String::from_utf8(buf).map_err(|_| to_err(format!("read_file: {:?} is not valid UTF-8", path)))
}

/// Faixa de linhas, 1-based e inclusiva nas duas pontas -- como editor e como
/// `sed -n 'i,jp'`, nao como slice de Rust. Aceita "120-180", "120-" (dali ate
/// o fim) e "120" (so aquela).
///
/// Existe porque `read_file` e a primitiva mais usada da ferramenta (20.729
/// chamadas na telemetria do #58) e lia SEMPRE o arquivo inteiro: um script que
/// precisava de 20 linhas pagava o arquivo todo, e o que fosse impresso virava
/// token de contexto (#61).
///
/// Para em `fim` em vez de ler ate o EOF -- o ganho e nao materializar o resto,
/// nao so nao devolve-lo.
fn faixa_de_linhas(spec: &str) -> Result<(usize, usize), Box<EvalAltResult>> {
    let spec = spec.trim();
    let erro = || {
        to_err(format!(
            "read_file: lines {spec:?} is not a range -- use \"120-180\", \"120-\" or \"120\""
        ))
    };
    let (ini_txt, fim_txt) = match spec.split_once('-') {
        Some((a, "")) => (a, None),
        Some((a, b)) => (a, Some(b)),
        None => (spec, Some(spec)),
    };
    let ini: usize = ini_txt.trim().parse().map_err(|_| erro())?;
    if ini == 0 {
        return Err(to_err("read_file: lines is 1-based; line 0 does not exist"));
    }
    let fim = match fim_txt {
        Some(b) => {
            let f: usize = b.trim().parse().map_err(|_| erro())?;
            if f < ini {
                return Err(to_err(format!(
                    "read_file: lines {spec:?} ends before it starts ({f} < {ini})"
                )));
            }
            f
        }
        None => usize::MAX,
    };
    Ok((ini, fim))
}

fn read_file_faixa(
    sandbox: &Sandbox,
    path: &str,
    spec: &str,
) -> Result<String, Box<EvalAltResult>> {
    use std::io::BufRead;
    let (ini, fim) = faixa_de_linhas(spec)?;
    let resolved = sandbox.resolve(path).map_err(to_err)?;
    let f = fs::File::open(&resolved).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            to_err(format!("read_file: {:?} does not exist", path))
        } else {
            to_err(format!("read_file: {:?}: {e}", path))
        }
    })?;
    let mut out = String::new();
    for (i, linha) in std::io::BufReader::new(f).lines().enumerate() {
        let n = i + 1;
        if n < ini {
            continue;
        }
        if n > fim {
            break;
        }
        let linha =
            linha.map_err(|_| to_err(format!("read_file: {:?} is not valid UTF-8", path)))?;
        out.push_str(&linha);
        out.push('\n');
    }
    Ok(out)
}

// ---- write_file ----

fn write_file_impl(sandbox: &Sandbox, path: &str, content: &str) -> Result<(), Box<EvalAltResult>> {
    // Mutação (ou comando que pode mutar) invalida o cache de resolução (#37).
    sandbox.cache.invalidar();
    if sandbox.dry {
        eprintln!("codemode: [dry-run] write_file({path:?}, {} bytes)", content.len());
        return Ok(());
    }
    let resolved = sandbox.resolve(path).map_err(to_err)?;
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent).map_err(|e| to_err(format!("write_file: cannot create {:?}: {e}", parent)))?;
    }
    fs::write(&resolved, content).map_err(|e| to_err(format!("write_file: {:?}: {e}", path)))
}

/// A full overwrite that shrinks an existing file to less than this
/// fraction of its size is refused. On 2026-08-19 a batch script whose
/// in-script content assembly came out empty ran `write_file(f, atual +
/// bloco)` over 70 files and replaced every one of them with just
/// `bloco`: `write_file` replaces the whole file, so ANY upstream bug
/// producing a shorter string is silent, total data loss. The read side
/// already fails loud (`read_file` errors, never returns ""), so the
/// remaining hole was the write side trusting whatever it was handed.
/// Half is deliberately coarse: it never fires on a normal rewrite and
/// always fires on a wipe.
fn shrinks_dangerously(old_len: u64, new_len: u64) -> bool {
    old_len > 0 && new_len.saturating_mul(2) < old_len
}

fn write_file_guarded(sandbox: &Sandbox, path: &str, content: &str) -> Result<(), Box<EvalAltResult>> {
    // Mutação (ou comando que pode mutar) invalida o cache de resolução (#37).
    sandbox.cache.invalidar();
    let resolved = sandbox.resolve(path).map_err(to_err)?;
    if let Ok(meta) = fs::metadata(&resolved) {
        if meta.is_file() && shrinks_dangerously(meta.len(), content.len() as u64) {
            return Err(to_err(format!(
                "write_file: recusado — {:?} tem {} bytes e o conteúdo novo tem {} \
                 (menos da metade): isso apaga o arquivo em vez de atualizá-lo. \
                 Use append_file(path, texto) pra acrescentar, edit_file(path, velho, novo) \
                 pra trocar um trecho, ou write_file_force(path, conteudo) se a substituição \
                 total for mesmo intencional.",
                path,
                meta.len(),
                content.len()
            )));
        }
    }
    if sandbox.dry {
        eprintln!("codemode: [dry-run] write_file({path:?}, {} bytes)", content.len());
        return Ok(());
    }
    // Resolve UMA vez: antes, `write_file_guarded` resolvia e chamava
    // `write_file_impl`, que resolvia tudo de novo -- duas travessias de
    // sandbox por escrita (#43).
    if let Some(pai) = resolved.parent() {
        fs::create_dir_all(pai).map_err(|e| to_err(format!("write_file: {:?}: {e}", path)))?;
    }
    fs::write(&resolved, content).map_err(|e| to_err(format!("write_file: {:?}: {e}", path)))
}

// ---- append_file ----

/// The safe way to add to a file: no read step to get wrong, and nothing
/// existing can be lost. This is what a `read_file` + `write_file`
/// "append" loop should have been.
fn append_file_impl(sandbox: &Sandbox, path: &str, content: &str) -> Result<(), Box<EvalAltResult>> {
    // Mutação (ou comando que pode mutar) invalida o cache de resolução (#37).
    sandbox.cache.invalidar();
    if sandbox.dry {
        eprintln!("codemode: [dry-run] append_file({path:?}, {} bytes)", content.len());
        return Ok(());
    }
    use std::io::Write as _;
    let resolved = sandbox.resolve(path).map_err(to_err)?;
    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent).map_err(|e| to_err(format!("append_file: cannot create {:?}: {e}", parent)))?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&resolved)
        .map_err(|e| to_err(format!("append_file: {:?}: {e}", path)))?;
    f.write_all(content.as_bytes())
        .map_err(|e| to_err(format!("append_file: {:?}: {e}", path)))
}

// ---- edit_file ----

fn edit_file_impl(sandbox: &Sandbox, path: &str, old: &str, new: &str) -> Result<(), Box<EvalAltResult>> {
    // Mutação (ou comando que pode mutar) invalida o cache de resolução (#37).
    sandbox.cache.invalidar();
    if sandbox.dry {
        eprintln!("codemode: [dry-run] edit_file({path:?}, {} -> {} bytes)", old.len(), new.len());
        return Ok(());
    }
    if old.is_empty() {
        return Err(to_err("edit_file: `old` must not be empty"));
    }
    let content = read_file_impl(sandbox, path)?;
    let count = content.matches(old).count();
    if count == 0 {
        return Err(to_err(format!("edit_file: {:?} not found in {:?}", old, path)));
    }
    if count > 1 {
        return Err(to_err(format!(
            "edit_file: {:?} is not unique in {:?} ({} occurrences); provide more context",
            old, path, count
        )));
    }
    let updated = content.replacen(old, new, 1);
    write_file_impl(sandbox, path, &updated)
}

// ---- run_shell ----

/// Any of these appearing in the command string means it's not a single
/// plain invocation (pipeline, chain, substitution, redirect) -- those go
/// through `sh -c` as before. RTK's own subcommands take plain argv, not
/// shell syntax, so routing a pipeline through `rtk <first-word> ...`
/// would silently drop everything after the first metacharacter.
const SHELL_METACHARS: &[char] = &['|', '&', ';', '>', '<', '`', '$', '(', ')', '\n'];

/// First words worth routing through `rtk`: measured against this repo,
/// `rtk`'s own startup (~3.5ms, `rtk --help` alone) is already more than
/// small/fast commands (`grep -c` on two tiny files: ~2ms raw) cost
/// end-to-end -- routing those through RTK is a net latency loss for no
/// real output-size win, RTK's whole value is trimming *large* output
/// (test runners, build tools, dependency installs). Curated from RTK's
/// own rule categories (Tests/Build/Cargo/PackageManager carry the highest
/// avg-tokens-saved weight; Files/System are the lowest) rather than
/// attempting to introspect RTK's internal rule table at runtime.
const RTK_WORTH_ROUTING: &[&str] = &[
    "cargo", "npm", "npx", "pnpm", "go", "mvn", "dotnet",
    "pytest", "jest", "vitest", "phpunit", "rake", "docker", "kubectl", "git",
    // Added after inspecting real production scripts (PR
    // review scripts): `gh pr diff`/`gh pr view` output on a real PR can run
    // thousands of tokens, and rtk has a dedicated gh filter for exactly
    // this -- these were being missed entirely, running raw through sh -c.
    "gh", "glab",
    // `find`/`grep`: rtk's own global usage history (`rtk gain`, all
    // sessions on this machine, not codemode-specific) shows `rtk find` as
    // the single largest saver by far -- 2.5M of 2.7M total tokens saved,
    // from large directory-tree scans. codemode already has native
    // `glob()`/`grep()` primitives that don't need rtk at all and should be
    // preferred in a script -- but when a script reaches for `run_shell`
    // with real `find`/`grep` flags the primitives don't cover (predicates,
    // context lines, etc.), it deserves the same filtering the top-level
    // Bash tool's hook already gives raw `find`/`grep`, not silent raw
    // passthrough. NOT migrated in-process: unlike cargo_test/git/gh's
    // filters (thin wrappers around already-captured output), rtk's find
    // and grep are full reimplementations (own traversal/search engine via
    // `ignore`/`walkdir`/`rg`), not simple `&str -> String` functions --
    // exposing that cleanly is real, separate work, not a one-line `pub`.
    "find", "grep",
    // `ls`: rtk's global history shows 27+ real `rtk ls -la` calls at
    // 78.3% average savings (rtk HAS a real ls handler, unlike make/yarn/
    // gradle) -- a script's `run_shell("ls -la dir")` was running raw for
    // no reason. Routed via the rtk binary; not in-process (rtk's ls is
    // its own listing implementation, not an output filter).
    "ls",
    // `make` is back: rtk's CLI still has no make handler (thurionapp/
    // rtk#3), but that no longer matters -- ALL make invocations are now
    // handled by the in-process `rtk::filters::make_output` arm in
    // try_in_process_filter (built for exactly the real make-heavy
    // production usage observed), so the rtk-binary tier is never
    // reached for make and its zero-filter passthrough tax never paid.
    "make",
    // `yarn`, `gradle` deliberately NOT here: no real Commands:: handler
    // in rtk (thurionapp/rtk#3) AND zero real usage evidence in any
    // transcript -- routing them would be pure latency tax.
];

/// Tries the in-process (`rtk::filters::*`, zero `rtk`-binary spawn) path
/// for `words`. Returns `None` when this exact invocation isn't one of the
/// shapes an in-process filter actually covers -- the caller falls through
/// to the rtk-binary-spawn tier (`RTK_WORTH_ROUTING`) in that case, which
/// still filters correctly, just not at in-process speed. Returning `None`
/// here must never mean "give up on filtering" -- that would silently
/// regress a command from "filtered via rtk binary" to "raw, unfiltered".
///
/// `cargo test` filters output already captured by a generic spawn
/// (`rtk::filters::cargo_test` is a pure `&str -> String` function). `git
/// log`/`git diff`/`git status` don't work that way: rtk's git filters are
/// coupled to a specific invocation (`git log` needs a
/// `--pretty=format:...---END---` marker RTK injects; plain `git log`
/// output silently misparses without it), so `rtk::filters::git_*` spawn
/// git themselves with that exact shape -- only bare `git log`/`git
/// diff`/`git status` (optionally `git log -N`) match here; anything with
/// other flags returns `None` on purpose rather than risking a semantic
/// mismatch between what the script asked for and what the fixed-shape lib
/// function would actually run.
fn try_in_process_filter(words: &[String], sandbox: &Sandbox) -> Option<Result<String, Box<EvalAltResult>>> {
    let to_result = |r: Result<String, String>| match r {
        Ok(s) => Ok(s),
        Err(e) => Err(to_err(format!("run_shell: {e}"))),
    };

    match words {
        [cargo, sub] if cargo == "cargo" && sub == "test" => {
            let output = { let mut c = Command::new("cargo"); c.arg("test").current_dir(&sandbox.root); exec_com_deadline(c, sandbox.cmd_timeout, "cargo test") };
            let output = match output {
                Ok(o) => o,
                Err(e) => return Some(Err(to_err(format!("run_shell: failed to spawn cargo: {e}")))),
            };
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            let mut filtered = rtk::filters::cargo_test(&combined);
            if !output.status.success() {
                filtered.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
            }
            Some(Ok(filtered))
        }
        [git, sub] if git == "git" && sub == "diff" => Some(to_result(rtk::filters::git_diff(&sandbox.root, 500))),
        [git, sub] if git == "git" && sub == "status" => Some(to_result(rtk::filters::git_status(&sandbox.root))),
        [git, sub] if git == "git" && sub == "log" => Some(to_result(rtk::filters::git_log(&sandbox.root, 10))),
        [git, sub, limit_arg] if git == "git" && sub == "log" => limit_arg
            .strip_prefix('-')
            .and_then(|n| n.parse::<usize>().ok())
            .map(|n| to_result(rtk::filters::git_log(&sandbox.root, n))),
        // Everything after "gh pr diff" (PR number, `--repo owner/name`,
        // etc.) passes straight through to the real `gh` invocation -- no
        // fixed-arity restriction like the git arms above, since a bare
        // `gh pr diff` and `gh pr diff 77 --repo owner/repo` are
        // both real, common shapes and neither changes what the filter
        // (compact_diff) needs to do.
        [gh, pr, diff, rest @ ..] if gh == "gh" && pr == "pr" && diff == "diff" => {
            let extra: Vec<&str> = rest.iter().map(|s| s.as_str()).collect();
            Some(to_result(rtk::filters::gh_pr_diff(&sandbox.root, &extra)))
        }
        // npx is heavily used in real production transcripts --
        // second only to pnpm among JS tooling. Unlike pnpm (see the long
        // comment on RTK_WORTH_ROUTING for why pnpm's real highest-volume
        // shapes have no filter yet), npx's filter is real and simple: a
        // pure output-boilerplate strip, same one `npm run` uses.
        [npx, rest @ ..] if npx == "npx" && !rest.is_empty() => {
            let output = { let mut c = Command::new("npx"); c.args(rest).current_dir(&sandbox.root); exec_com_deadline(c, sandbox.cmd_timeout, "npx") };
            let output = match output {
                Ok(o) => o,
                Err(e) => return Some(Err(to_err(format!("run_shell: failed to spawn npx: {e}")))),
            };
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            let mut filtered = rtk::filters::npx(&combined);
            if !output.status.success() {
                filtered.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
            }
            Some(Ok(filtered))
        }
        // `pnpm test`/`pnpm run <script>` (and the npm equivalents) --
        // pnpm's highest-volume real shapes in production transcripts,
        // previously blocked on rtk's CLI having no filter for them
        // (thurionapp/rtk#3). Unblocked by building the filter in our own
        // fork: `rtk::filters::pnpm_run` is vitest-aware (the runner these
        // scripts actually invoke, per real production usage) with a
        // conservative npm-boilerplate-strip fallback for non-test output.
        // All args pass through to the real pnpm/npm invocation untouched.
        [pm, sub, rest @ ..] if (pm == "pnpm" || pm == "npm") && (sub == "test" || sub == "run") => {
            let output = { let mut c = Command::new(pm.as_str()); c.arg(sub.as_str()).args(rest).current_dir(&sandbox.root); exec_com_deadline(c, sandbox.cmd_timeout, pm.as_str()) };
            let output = match output {
                Ok(o) => o,
                Err(e) => return Some(Err(to_err(format!("run_shell: failed to spawn {pm}: {e}")))),
            };
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            let mut filtered = rtk::filters::pnpm_run(&combined);
            if !output.status.success() {
                filtered.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
            }
            Some(Ok(filtered))
        }
        // `make <targets...>` -- rtk's CLI has no make handler at all, so
        // this arm intentionally matches EVERY make invocation (the
        // rtk-binary tier would be a zero-filter passthrough tax, see the
        // RTK_WORTH_ROUTING comment). The filter is conservative by
        // design: collapses embedded cargo compile-progress runs, drops
        // Entering/Leaving chatter, passes everything else through.
        [make, rest @ ..] if make == "make" => {
            let output = { let mut c = Command::new("make"); c.args(rest).current_dir(&sandbox.root); exec_com_deadline(c, sandbox.cmd_timeout, "make") };
            let output = match output {
                Ok(o) => o,
                Err(e) => return Some(Err(to_err(format!("run_shell: failed to spawn make: {e}")))),
            };
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            let mut filtered = rtk::filters::make_output(&combined);
            if !output.status.success() {
                filtered.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
            }
            Some(Ok(filtered))
        }
        _ => None,
    }
}

/// `rtk grep` with a format/shape flag (`-c`, `-l`, `-L`, `-o`, `-q` and
/// their long forms) is a guaranteed passthrough: rtk's own search.rs
/// checks `has_format_flag` and re-runs the engine verbatim, filtering
/// nothing -- the agent already chose a minimal output shape. Routing
/// those through rtk is the pure-latency-tax mistake again (~7ms of rtk
/// spawn for byte-identical output), found by re-running the benchmark:
/// `bump_version.rhai`'s tiny `grep -c` verification regressed 7.5ms ->
/// 13.9ms the day grep entered RTK_WORTH_ROUTING. Mirrors rtk's own flag
/// list; short flags are scanned per-letter so clusters like `-rlc` count.
/// Formas cujo roteamento pelo RTK so cobra e nao entrega (#32).
///
/// Medido: comando roteado sem filtro in-process paga **+2,8ms** pelo spawn
/// do binario `rtk`. Faz sentido quando a saida e grande e o filtro corta
/// muito -- `cargo test`, `git diff`. Nao faz sentido nestes dois casos:
///
/// 1. **Saida ja minima**: `git rev-parse HEAD` sao 40 caracteres. Nao ha
///    o que comprimir, so o que pagar.
/// 2. **Saida legivel por maquina**: `gh ... --json ... --jq ...` devolve
///    JSON que o script vai parsear. Passar isso por um filtro de texto e,
///    na melhor hipotese, inutil.
fn saida_ja_e_minima(words: &[String]) -> bool {
    let tem = |flag: &str| words.iter().any(|w| w == flag);
    if tem("--json") || tem("--jq") || tem("--oneline") || tem("--porcelain") || tem("--quiet") {
        return true;
    }
    let sub = words.get(1).map(|s| s.as_str()).unwrap_or("");
    matches!(words[0].as_str(), "git")
        && matches!(
            sub,
            "rev-parse" | "config" | "symbolic-ref" | "remote" | "worktree" | "check-ignore"
        )
}

fn grep_shape_rtk_would_passthrough(words: &[String]) -> bool {
    if words.first().map(|w| w.as_str()) != Some("grep") {
        return false;
    }
    const LONG: &[&str] = &[
        "--count",
        "--count-matches",
        "--files-with-matches",
        "--files-without-match",
        "--only-matching",
        "--quiet",
        "--silent",
    ];
    words[1..].iter().any(|w| {
        LONG.contains(&w.as_str())
            || (w.starts_with('-')
                && !w.starts_with("--")
                && w[1..].chars().any(|c| matches!(c, 'c' | 'l' | 'L' | 'o' | 'q')))
    })
}


/// Comando de shell trivial resolvido em processo (#29).
///
/// O censo de 401 comandos reais escritos por scripts mostrou que **55% sao
/// coisas triviais** -- cat, ls, grep, rm, cp, echo, test, mkdir, touch,
/// head, basename, dirname, true -- e cada uma delas custa 1,5ms a 8,5ms de
/// spawn contra 0,02ms feita aqui dentro.
///
/// Regra de ouro: **so a forma exata**. Qualquer flag fora da lista, glob,
/// metachar ou aridade diferente devolve None e cai no spawn de sempre --
/// divergir do shell de verdade seria pior que ser lento. Todo caminho passa
/// pelo sandbox, igual as primitivas.
fn try_comando_nativo(words: &[String], sandbox: &Sandbox) -> Option<Result<String, Box<EvalAltResult>>> {
    fn ok(s: String) -> Option<Result<String, Box<EvalAltResult>>> {
        Some(Ok(s))
    }
    fn falha(codigo: i32) -> Option<Result<String, Box<EvalAltResult>>> {
        Some(Ok(format!("\n[exit code: {codigo}]")))
    }
    let ler = |p: &str| -> Option<String> {
        let alvo = sandbox.resolve(p).ok()?;
        fs::read_to_string(alvo).ok()
    };

    let partes: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    match partes.as_slice() {
        ["true"] => ok(String::new()),
        ["false"] => falha(1),
        ["pwd"] => ok(format!("{}\n", sandbox.root.display())),

        ["echo", resto @ ..] => ok(format!("{}\n", resto.join(" "))),

        ["cat", arquivos @ ..] if !arquivos.is_empty() && !arquivos.iter().any(|a| a.starts_with('-')) => {
            let mut saida = String::new();
            for a in arquivos {
                // `?` aqui devolve None e manda o comando pro shell: sem
                // inventar mensagem de erro do cat, que o chamador espera no
                // formato do binario real.
                saida.push_str(&ler(a)?);
            }
            ok(saida)
        }

        ["basename", caminho] => {
            ok(format!("{}\n", std::path::Path::new(caminho).file_name()?.to_string_lossy()))
        }
        ["dirname", caminho] => {
            let pai = std::path::Path::new(caminho).parent()?;
            let texto = pai.to_string_lossy();
            ok(format!("{}\n", if texto.is_empty() { "." } else { &texto }))
        }

        ["ls"] | ["ls", "-1"] => listar(sandbox, "."),
        ["ls", dir] | ["ls", "-1", dir] if !dir.starts_with('-') => listar(sandbox, dir),

        ["test", "-f", alvo] => {
            let existe = sandbox.resolve(alvo).map(|p| p.is_file()).unwrap_or(false);
            if existe { ok(String::new()) } else { falha(1) }
        }
        ["test", "-d", alvo] => {
            let existe = sandbox.resolve(alvo).map(|p| p.is_dir()).unwrap_or(false);
            if existe { ok(String::new()) } else { falha(1) }
        }
        ["test", "-e", alvo] => {
            let existe = sandbox.resolve(alvo).map(|p| p.exists()).unwrap_or(false);
            if existe { ok(String::new()) } else { falha(1) }
        }

        ["head", "-n", n, arquivo] => {
            let limite: usize = n.parse().ok()?;
            let conteudo = ler(arquivo)?;
            let mut saida: String =
                conteudo.lines().take(limite).map(|l| format!("{l}\n")).collect();
            if saida.is_empty() && !conteudo.is_empty() {
                saida.push('\n');
            }
            ok(saida)
        }

        ["mkdir", "-p", dir] => {
            let alvo = sandbox.resolve(dir).ok()?;
            match fs::create_dir_all(alvo) {
                Ok(()) => ok(String::new()),
                Err(_) => None,
            }
        }
        ["touch", arquivo] => {
            let alvo = sandbox.resolve(arquivo).ok()?;
            if alvo.exists() {
                // `touch` num arquivo existente so mexe no mtime; deixa pro
                // shell, que faz isso direito.
                return None;
            }
            match fs::write(&alvo, "") {
                Ok(()) => ok(String::new()),
                Err(_) => None,
            }
        }
        ["rm", arquivo] | ["rm", "-f", arquivo] => {
            let alvo = sandbox.resolve(arquivo).ok()?;
            if alvo.is_dir() {
                return None;
            }
            match fs::remove_file(&alvo) {
                Ok(()) => ok(String::new()),
                Err(_) if partes[1] == "-f" => ok(String::new()),
                Err(_) => None,
            }
        }
        ["cp", origem, destino] => {
            let de = sandbox.resolve(origem).ok()?;
            let para = sandbox.resolve(destino).ok()?;
            if de.is_dir() || para.is_dir() {
                return None;
            }
            match fs::copy(&de, &para) {
                Ok(_) => ok(String::new()),
                Err(_) => None,
            }
        }
        ["mv", origem, destino] => {
            let de = sandbox.resolve(origem).ok()?;
            let para = sandbox.resolve(destino).ok()?;
            if para.is_dir() {
                return None;
            }
            match fs::rename(&de, &para) {
                Ok(()) => ok(String::new()),
                Err(_) => None,
            }
        }

        // `grep -rn PADRAO CAMINHO`: mesma forma de saida do nosso grep
        // nativo (caminho:linha:texto), que agora respeita .gitignore.
        ["grep", flags, padrao, caminho] if flags == &"-rn" || flags == &"-nr" => {
            Some(grep_impl(sandbox, padrao, caminho))
        }
        _ => None,
    }
}

fn listar(sandbox: &Sandbox, dir: &str) -> Option<Result<String, Box<EvalAltResult>>> {
    let alvo = sandbox.resolve(dir).ok()?;
    let entradas = fs::read_dir(alvo).ok()?;
    let mut nomes: Vec<String> = entradas
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        // `ls` sem -a nao mostra oculto.
        .filter(|n| !n.starts_with('.'))
        .collect();
    nomes.sort();
    Some(Ok(nomes.into_iter().map(|n| format!("{n}\n")).collect()))
}


// ---------------------------------------------------------------------------
// Pipeline sem `sh` (#30)
//
// Medido: `run_shell("sh -c 'cat a.txt'")` custa 4,55ms contra 1,66ms do
// spawn direto -- o `sh` intermediario e 3x o custo do comando. E 20% dos
// comandos reais tem metachar; destes, 63 de 85 sao pipe.
//
// A regra e a mesma do caminho nativo: so o subconjunto que da pra executar
// com semantica IDENTICA. Qualquer sinal de shell de verdade -- expansao,
// subshell, glob, `;`, `&&`, variavel -- devolve None e vai pro `sh`, que
// e quem sabe fazer aquilo direito.
// ---------------------------------------------------------------------------

/// Sinais de que so o shell resolve: presentes, nem tentamos.
const SO_O_SHELL_RESOLVE: &[char] =
    &['$', '`', ';', '(', ')', '<', '*', '?', '[', ']', '{', '}', '~', '\\', '\n'];

struct Pipeline {
    etapas: Vec<Vec<String>>,
    /// (caminho, append) do `>` ou `>>` final.
    redireciona: Option<(String, bool)>,
    /// `2>&1` na ultima etapa: junta stderr no stdout.
    junta_stderr: bool,
}

fn parse_pipeline(cmd: &str) -> Option<Pipeline> {
    let mut resto = cmd.trim().to_string();
    let junta_stderr = resto.contains("2>&1");
    if junta_stderr {
        resto = resto.replace("2>&1", " ");
    }
    if resto.contains("&&") || resto.contains("||") || resto.contains('&') {
        return None;
    }
    if resto.chars().any(|c| SO_O_SHELL_RESOLVE.contains(&c)) {
        return None;
    }

    let mut redireciona = None;
    if let Some(pos) = resto.rfind('>') {
        let append = pos > 0 && resto.as_bytes()[pos - 1] == b'>';
        let inicio = if append { pos - 1 } else { pos };
        let alvo = resto[pos + 1..].trim().to_string();
        if alvo.is_empty() || alvo.contains('>') || alvo.split_whitespace().count() != 1 {
            return None;
        }
        redireciona = Some((alvo, append));
        resto.truncate(inicio);
    }
    if resto.contains('>') {
        return None;
    }

    let mut etapas = Vec::new();
    for pedaco in resto.split('|') {
        let palavras = shell_words::split(pedaco).ok()?;
        if palavras.is_empty() {
            return None;
        }
        etapas.push(palavras);
    }
    if etapas.is_empty() || (etapas.len() == 1 && redireciona.is_none()) {
        return None;
    }
    Some(Pipeline { etapas, redireciona, junta_stderr })
}

/// Etapa que consome stdin e da pra fazer em memoria -- o padrao real e
/// `algo | head -20`, `algo | wc -l`, `algo | sort`.
fn filtro_em_memoria(palavras: &[String], entrada: &str) -> Option<String> {
    let p: Vec<&str> = palavras.iter().map(|s| s.as_str()).collect();
    match p.as_slice() {
        ["cat"] => Some(entrada.to_string()),
        ["head"] => Some(entrada.lines().take(10).map(|l| format!("{l}\n")).collect()),
        ["head", "-n", n] => {
            let limite: usize = n.parse().ok()?;
            Some(entrada.lines().take(limite).map(|l| format!("{l}\n")).collect())
        }
        ["tail", "-n", n] => {
            let limite: usize = n.parse().ok()?;
            let linhas: Vec<&str> = entrada.lines().collect();
            let inicio = linhas.len().saturating_sub(limite);
            Some(linhas[inicio..].iter().map(|l| format!("{l}\n")).collect())
        }
        ["wc", "-l"] => Some(format!("{}\n", entrada.lines().count())),
        ["wc", "-c"] => Some(format!("{}\n", entrada.len())),
        ["sort"] => {
            let mut linhas: Vec<&str> = entrada.lines().collect();
            linhas.sort_unstable();
            Some(linhas.iter().map(|l| format!("{l}\n")).collect())
        }
        ["sort", "-u"] => {
            let mut linhas: Vec<&str> = entrada.lines().collect();
            linhas.sort_unstable();
            linhas.dedup();
            Some(linhas.iter().map(|l| format!("{l}\n")).collect())
        }
        ["uniq"] => {
            let mut saida = String::new();
            let mut anterior: Option<&str> = None;
            for l in entrada.lines() {
                if Some(l) != anterior {
                    saida.push_str(l);
                    saida.push('\n');
                }
                anterior = Some(l);
            }
            Some(saida)
        }
        ["grep", padrao] => {
            Some(entrada.lines().filter(|l| l.contains(padrao)).map(|l| format!("{l}\n")).collect())
        }
        ["grep", "-v", padrao] => {
            Some(entrada.lines().filter(|l| !l.contains(padrao)).map(|l| format!("{l}\n")).collect())
        }
        ["grep", "-c", padrao] => {
            Some(format!("{}\n", entrada.lines().filter(|l| l.contains(padrao)).count()))
        }
        _ => None,
    }
}

fn roda_etapa_com_entrada(
    palavras: &[String],
    entrada: &str,
    sandbox: &Sandbox,
    junta_stderr: bool,
) -> Option<String> {
    if let Some(saida) = filtro_em_memoria(palavras, entrada) {
        return Some(saida);
    }
    use std::io::Write;
    use std::process::Stdio;
    let mut c = Command::new(&palavras[0]);
    c.args(&palavras[1..])
        .current_dir(&sandbox.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if junta_stderr { Stdio::piped() } else { Stdio::null() });
    let mut filho = c.spawn().ok()?;
    if let Some(mut stdin) = filho.stdin.take() {
        let dados = entrada.as_bytes().to_vec();
        // Escreve numa thread: pipeline com muita entrada trava se o
        // processo comecar a escrever antes de a gente terminar de mandar.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&dados);
        });
    }
    let saida = filho.wait_with_output().ok()?;
    let mut texto = String::from_utf8_lossy(&saida.stdout).to_string();
    if junta_stderr {
        texto.push_str(&String::from_utf8_lossy(&saida.stderr));
    }
    Some(texto)
}

fn tenta_pipeline(cmd: &str, sandbox: &Sandbox) -> Option<Result<String, Box<EvalAltResult>>> {
    let pipeline = parse_pipeline(cmd)?;
    // Denylist vale por etapa: um `rm -rf /` no meio do pipe e um `rm -rf /`.
    for etapa in &pipeline.etapas {
        if let Some(rule) = denylist::check(&etapa.join(" ")) {
            return Some(Err(Box::new(EvalAltResult::ErrorTerminated(
                format!("denylist:run_shell refused, command matches denylist rule '{rule}'").into(),
                rhai::Position::NONE,
            ))));
        }
    }

    let mut atual = String::new();
    for (i, etapa) in pipeline.etapas.iter().enumerate() {
        let ultima = i == pipeline.etapas.len() - 1;
        let junta = pipeline.junta_stderr && ultima;
        if i == 0 {
            atual = match try_comando_nativo(etapa, sandbox) {
                Some(Ok(saida)) => saida,
                Some(Err(e)) => return Some(Err(e)),
                None => {
                    let mut c = Command::new(&etapa[0]);
                    c.args(&etapa[1..]).current_dir(&sandbox.root);
                    let saida = exec_com_deadline(c, sandbox.cmd_timeout, cmd).ok()?;
                    let mut texto = String::from_utf8_lossy(&saida.stdout).to_string();
                    if junta {
                        texto.push_str(&String::from_utf8_lossy(&saida.stderr));
                    }
                    texto
                }
            };
        } else {
            atual = roda_etapa_com_entrada(etapa, &atual, sandbox, junta)?;
        }
    }

    if let Some((destino, append)) = pipeline.redireciona {
        let alvo = sandbox.resolve(&destino).ok()?;
        let escrita = if append {
            fs::OpenOptions::new().create(true).append(true).open(&alvo).and_then(|mut f| {
                use std::io::Write;
                f.write_all(atual.as_bytes())
            })
        } else {
            fs::write(&alvo, atual.as_bytes())
        };
        return match escrita {
            Ok(()) => Some(Ok(String::new())),
            Err(_) => None,
        };
    }
    Some(Ok(atual))
}

fn run_shell_impl(sandbox: &Sandbox, cmd: &str, confirm: bool) -> Result<String, Box<EvalAltResult>> {
    // Mutação (ou comando que pode mutar) invalida o cache de resolução (#37).
    sandbox.cache.invalidar();
    if sandbox.dry {
        eprintln!("codemode: [dry-run] run_shell({cmd:?})");
        return Ok(String::new());
    }
    if let Some(rule) = denylist::check(cmd) {
        if !confirm {
            // ErrorTerminated, not a plain runtime error, on purpose: Rhai
            // scripts can `try`/`catch` runtime errors, and a model-written
            // script that swallows the refusal can hammer the denylist in a
            // loop inside one Bash call (the exact failure DeepSeek Harness
            // shipped: a policy denial surfaced as a catchable ToolCallError,
            // their discussion #532). ErrorTerminated is the one Rhai error
            // try/catch cannot intercept -- a denylist hit aborts the whole
            // script. main.rs tells it apart from the timeout terminator by
            // the "denylist:" token prefix.
            return Err(Box::new(EvalAltResult::ErrorTerminated(
                format!(
                    "denylist:run_shell refused, command matches denylist rule '{rule}'. Pass confirm:true to override if you really mean it."
                )
                .into(),
                rhai::Position::NONE,
            )));
        }
    }

    // A plain single command (no pipes/chains/redirects/substitution)
    // whose first word is a known-heavy tool gets routed for the same
    // output trimming the top-level Bash tool already gets -- otherwise a
    // script's run_shell call silently bypasses RTK entirely and ships raw
    // output (e.g. a full `cargo test` log instead of RTK's ~99.6%-smaller
    // failures-only summary) back into the model's context. Two tiers:
    // an in-process filter call when one's been ported (fastest, zero rtk
    // spawn), otherwise the `rtk` binary itself (still faster than raw for
    // large-output commands, just not as fast as in-process). No separate
    // `rtk --help` availability probe: that was a second full RTK process
    // spawn on top of the routed call itself, doubling the cost it exists
    // to avoid (found by profiling, not assumed) -- instead just attempt
    // the routed spawn and fall back to sh -c only if the OS itself
    // reports the target binary isn't found.
    // Any plain command (no metachars) gets split once; routing decisions
    // hang off the split. Plain-but-unrouted commands used to go through
    // `sh -c` anyway, paying TWO process spawns (sh, then the real
    // command) -- measured at ~3.7ms of pure sh overhead per run_shell
    // call on this machine (`run_shell("true")` cost 8.5ms total against
    // a 4.8ms noop baseline). Now they spawn the target directly, with
    // `sh -c` kept only as the fallback for shell syntax, shell builtins,
    // and anything the direct spawn can't find.
    // Pipeline simples e redirecionamento resolvidos sem o `sh` no meio
    // (#30). Qualquer coisa fora do subconjunto seguro devolve None aqui e
    // segue o caminho de sempre.
    if !sandbox.dry {
        if let Some(resultado) = tenta_pipeline(cmd, sandbox) {
            return resultado;
        }
    }

    let plain = if SHELL_METACHARS.iter().any(|c| cmd.contains(*c)) {
        None
    } else {
        shell_words::split(cmd).ok().filter(|w| !w.is_empty())
    };
    let routed = plain.as_ref().is_some_and(|w| {
        w.first().map(|first| RTK_WORTH_ROUTING.contains(&first.as_str())).unwrap_or(false)
            && !grep_shape_rtk_would_passthrough(w)
            && !saida_ja_e_minima(w)
    });

    // Antes de qualquer spawn: o comando trivial resolvido aqui dentro custa
    // 0,02ms contra 1,5-8,5ms de processo (#29). So a forma exata entra.
    if let Some(words) = &plain {
        if !sandbox.dry {
            if let Some(resultado) = try_comando_nativo(words, sandbox) {
                return resultado;
            }
        }
    }

    if routed {
        if let Some(words) = &plain {
            if let Some(result) = try_in_process_filter(words, sandbox) {
                return result;
            }
        }
    }

    let t = sandbox.cmd_timeout;
    let sh_fallback = || {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd).current_dir(&sandbox.root);
        exec_com_deadline(c, t, cmd)
    };
    let output = match &plain {
        Some(words) if routed => {
            let mut c = Command::new("rtk");
            c.args(words).current_dir(&sandbox.root);
            match exec_com_deadline(c, t, cmd) {
            Ok(o) => Ok(o),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => sh_fallback(),
            Err(e) => Err(e),
        }}
        Some(words) => {
            let mut c = Command::new(&words[0]);
            c.args(&words[1..]).current_dir(&sandbox.root);
            match exec_com_deadline(c, t, cmd) {
            Ok(o) => Ok(o),
            // NotFound covers shell builtins/functions (`command`, `type`,
            // aliases) that only exist inside a shell -- hand those to sh.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => sh_fallback(),
            Err(e) => Err(e),
        }}
        None => sh_fallback(),
    }
    .map_err(|e| to_err(format!("run_shell: {e}")))?;

    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        combined.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
    }
    Ok(combined)
}

/// Typed sibling of `run_shell` (issue #6, ported from DeepSeek Harness's
/// typed-tool-returns): returns a map `#{stdout, stderr, exit_code,
/// success}` so a script can BRANCH on results programmatically instead of
/// scraping prose. Deliberately does NOT go through the RTK filter tiers:
/// compression exists for text headed back into the model's context, and
/// merging/condensing streams would destroy exactly the raw fields this
/// function's contract promises. Rule of thumb for scripts: output you
/// print/return -> `run_shell` (filtered); output you branch on ->
/// `run_shell_full` (raw, typed). Same denylist gate, same uncatchable
/// refusal.
fn run_shell_full_impl(sandbox: &Sandbox, cmd: &str, confirm: bool) -> Result<Map, Box<EvalAltResult>> {
    // Mutação (ou comando que pode mutar) invalida o cache de resolução (#37).
    sandbox.cache.invalidar();
    if sandbox.dry {
        eprintln!("codemode: [dry-run] run_shell_full({cmd:?})");
        let mut m = Map::new();
        m.insert("stdout".into(), String::new().into());
        m.insert("stderr".into(), String::new().into());
        m.insert("exit_code".into(), 0_i64.into());
        m.insert("success".into(), true.into());
        m.insert("dry".into(), true.into());
        return Ok(m);
    }
    if let Some(rule) = denylist::check(cmd) {
        if !confirm {
            return Err(Box::new(EvalAltResult::ErrorTerminated(
                format!(
                    "denylist:run_shell_full refused, command matches denylist rule '{rule}'. Pass confirm:true to override if you really mean it."
                )
                .into(),
                rhai::Position::NONE,
            )));
        }
    }

    let plain = if SHELL_METACHARS.iter().any(|c| cmd.contains(*c)) {
        None
    } else {
        shell_words::split(cmd).ok().filter(|w| !w.is_empty())
    };
    let t = sandbox.cmd_timeout;
    let sh_fallback = || {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd).current_dir(&sandbox.root);
        exec_com_deadline(c, t, cmd)
    };
    let output = match &plain {
        Some(words) => {
            let mut c = Command::new(&words[0]);
            c.args(&words[1..]).current_dir(&sandbox.root);
            match exec_com_deadline(c, t, cmd) {
                Ok(o) => Ok(o),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => sh_fallback(),
                Err(e) => Err(e),
            }
        }
        None => sh_fallback(),
    }
    .map_err(|e| to_err(format!("run_shell_full: {e}")))?;

    let mut map = Map::new();
    map.insert("stdout".into(), String::from_utf8_lossy(&output.stdout).into_owned().into());
    map.insert("stderr".into(), String::from_utf8_lossy(&output.stderr).into_owned().into());
    map.insert(
        "exit_code".into(),
        rhai::Dynamic::from(output.status.code().unwrap_or(-1) as i64),
    );
    map.insert("success".into(), output.status.success().into());
    Ok(map)
}

// ---- http_get ----

/// Splits an http(s) URL into (host, explicit port, is_https). Deliberately
/// tiny and strict -- every branch this DOESN'T support (other schemes,
/// userinfo, bracketed IPv6) is refused loudly rather than half-parsed,
/// because a mis-parsed host is an allowlist bypass (the Check Point
/// lesson: every native API reachable from untrusted script code is part
/// of the security boundary).
fn parse_http_host(url: &str) -> Result<(String, Option<u16>, bool), String> {
    let (rest, https) = if let Some(r) = url.strip_prefix("https://") {
        (r, true)
    } else if let Some(r) = url.strip_prefix("http://") {
        (r, false)
    } else {
        return Err(format!("http_get: only http:// and https:// URLs are supported, got {url:?}"));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err(format!("http_get: URL {url:?} has no host"));
    }
    if authority.contains('@') {
        return Err(format!("http_get: userinfo (user@host) in URL is not supported: {url:?}"));
    }
    if authority.starts_with('[') {
        return Err(format!("http_get: bracketed IPv6 hosts are not supported: {url:?}"));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            let port: u16 = p
                .parse()
                .map_err(|_| format!("http_get: invalid port {p:?} in {url:?}"))?;
            Ok((h.to_ascii_lowercase(), Some(port), https))
        }
        Some((_, p)) => Err(format!("http_get: invalid port {p:?} in {url:?}")),
        None => Ok((authority.to_ascii_lowercase(), None, https)),
    }
}

fn host_allowed(allow: &[String], host: &str, port: Option<u16>, https: bool) -> bool {
    let effective = port.unwrap_or(if https { 443 } else { 80 });
    let with_port = format!("{host}:{effective}");
    allow
        .iter()
        .map(|a| a.to_ascii_lowercase())
        .any(|a| a == host || a == with_port)
}

/// `http_get(url) -> #{status, body, success}` gated by a static host
/// allowlist (`codemode run --allow-host <h>`, repeatable). Default-closed:
/// no flag, no network -- and a disallowed host is the same uncatchable
/// ErrorTerminated as a denylist hit, so a script can't probe hosts in a
/// try/catch loop. Fetching is delegated to `curl` with a fixed argv (no
/// shell, no config file, no redirect following -- a 3xx comes back as the
/// status itself, so a redirect can never hop to a host that was never
/// allowed) rather than embedding an HTTP+TLS stack into the binary: less
/// native surface, not more, per the boundary lesson above.
fn http_get_impl(allow: &[String], url: &str) -> Result<Map, Box<EvalAltResult>> {
    let (host, port, https) = parse_http_host(url).map_err(to_err)?;
    if !host_allowed(allow, &host, port, https) {
        return Err(Box::new(EvalAltResult::ErrorTerminated(
            format!(
                "denylist:http_get refused, host '{host}' is not in the --allow-host allowlist (default-closed: pass --allow-host {host} to `codemode run` to permit it)"
            )
            .into(),
            rhai::Position::NONE,
        )));
    }

    let body_path = std::env::temp_dir().join(format!(
        "codemode-http-{}-{}.body",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let output = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "30",
            "-o",
            &body_path.to_string_lossy(),
            "-w",
            "%{http_code}",
            "--",
            url,
        ])
        .output()
        .map_err(|e| to_err(format!("http_get: failed to spawn curl: {e}")))?;

    let read_body = || -> Result<String, Box<EvalAltResult>> {
        const MAX_BODY: u64 = 10 * 1024 * 1024;
        let meta = match fs::metadata(&body_path) {
            Ok(m) => m,
            Err(_) => return Ok(String::new()),
        };
        if meta.len() > MAX_BODY {
            let _ = fs::remove_file(&body_path);
            return Err(to_err(format!(
                "http_get: response body is {} bytes, over the {MAX_BODY}-byte cap",
                meta.len()
            )));
        }
        let bytes = fs::read(&body_path).unwrap_or_default();
        let _ = fs::remove_file(&body_path);
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };

    if !output.status.success() {
        let _ = read_body();
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(to_err(format!("http_get: curl failed for {url:?}: {}", stderr.trim())));
    }

    let status: i64 = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0);
    let body = read_body()?;
    let mut map = Map::new();
    map.insert("status".into(), rhai::Dynamic::from(status));
    map.insert("body".into(), body.into());
    map.insert("success".into(), (200..300).contains(&status).into());
    Ok(map)
}

fn confirm_from_map(opts: &Map) -> bool {
    opts.get("confirm").map(|d| d.as_bool().unwrap_or(false)).unwrap_or(false)
}

// ---- grep ----

/// Busca nativa. Antes desta versao o walker descia a arvore inteira pulando
/// so `.git`, lia cada arquivo como UTF-8 (binario incluido, que so falhava
/// DEPOIS de ser lido inteiro) e rodava numa thread so: no proprio repo isso
/// custava 2.590ms -- quatro vezes mais lento que sair pro shell, que era
/// exatamente o que a primitiva existia pra evitar (#28).
///
/// Agora usa o walker do ripgrep (crate `ignore`, ja no grafo via rtk):
/// respeita `.gitignore`, pula `.git/`, farejando binario pelos primeiros
/// bytes ANTES de ler o arquivo inteiro, em paralelo. A saida sai ordenada
/// por caminho de proposito -- walker paralelo devolve fora de ordem, e
/// resultado de busca precisa ser reproduzivel.
fn grep_fallback(sandbox: &Sandbox, pattern: &str, start: &std::path::Path) -> String {
    use std::io::Read;
    use std::sync::Mutex;

    let achados: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let raiz = sandbox.root.clone();
    let alvo = pattern.to_string();

    let mut construtor = ignore::WalkBuilder::new(start);
    construtor
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .hidden(false)
        .threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8));

    construtor.build_parallel().run(|| {
        let achados = &achados;
        let raiz = raiz.clone();
        let alvo = alvo.clone();
        Box::new(move |resultado| {
            let Ok(entrada) = resultado else {
                return ignore::WalkState::Continue;
            };
            if entrada.file_name() == ".git" {
                return ignore::WalkState::Skip;
            }
            if !entrada.file_type().is_some_and(|t| t.is_file()) {
                return ignore::WalkState::Continue;
            }
            let caminho = entrada.path();
            let Ok(mut f) = fs::File::open(caminho) else {
                return ignore::WalkState::Continue;
            };
            // Fareja binario nos primeiros 8 KB antes de ler o resto: sem
            // isto, um .rlib de 300 MB era lido inteiro so pra ser descartado.
            let mut inicio = [0u8; 8192];
            let Ok(lidos) = f.read(&mut inicio) else {
                return ignore::WalkState::Continue;
            };
            if inicio[..lidos].contains(&0) {
                return ignore::WalkState::Continue;
            }
            let mut resto = Vec::new();
            if f.read_to_end(&mut resto).is_err() {
                return ignore::WalkState::Continue;
            }
            let mut bytes = inicio[..lidos].to_vec();
            bytes.extend_from_slice(&resto);
            let Ok(conteudo) = String::from_utf8(bytes) else {
                return ignore::WalkState::Continue;
            };
            let rel = caminho.strip_prefix(&raiz).unwrap_or(caminho).display().to_string();
            let mut locais = String::new();
            for (i, linha) in conteudo.lines().enumerate() {
                if linha.contains(&alvo) {
                    locais.push_str(&format!("{}:{}:{}\n", rel, i + 1, linha));
                }
            }
            if !locais.is_empty() {
                if let Ok(mut acc) = achados.lock() {
                    acc.push((rel, locais));
                }
            }
            ignore::WalkState::Continue
        })
    });

    let mut acc = achados.into_inner().unwrap_or_default();
    acc.sort_by(|a, b| a.0.cmp(&b.0));
    acc.into_iter().map(|(_, texto)| texto).collect()
}

fn grep_impl(sandbox: &Sandbox, pattern: &str, path: &str) -> Result<String, Box<EvalAltResult>> {
    let resolved = sandbox.resolve(path).map_err(to_err)?;
    let dbg = std::env::var("CODEMODE_DEBUG_GREP").is_ok();
    let t = std::time::Instant::now();
    let r = { let mut c = Command::new("rg"); c.arg("-n").arg("--no-heading").arg(pattern).arg(&resolved); exec_com_deadline(c, sandbox.cmd_timeout, "rg") };
    if dbg { eprintln!("DBG tentativa de rg: {:?} em {:.1}ms", r.as_ref().map(|_| "ok").map_err(|e| e.kind()), t.elapsed().as_secs_f64()*1000.0); }
    match r {
        Ok(output) => {
            let mut combined = String::new();
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            Ok(combined)
        }
        Err(_) => Ok(grep_fallback(sandbox, pattern, &resolved)),
    }
}

// ---- glob ----

/// Prefixo literal do padrao: tudo antes do primeiro curinga. E o que o
/// chamador pediu EXPLICITAMENTE, e por isso manda mais que o .gitignore.
fn prefixo_literal(pattern: &str) -> &str {
    match pattern.find(['*', '?', '[']) {
        Some(i) => match pattern[..i].rfind('/') {
            Some(barra) => &pattern[..barra],
            None => "",
        },
        None => pattern,
    }
}

/// O caminho esta sob algo que o .gitignore exclui?
fn caminho_ignorado(raiz: &std::path::Path, alvo: &std::path::Path) -> bool {
    let Ok(gitignore) = ignore::gitignore::GitignoreBuilder::new(raiz).build() else {
        return false;
    };
    let mut atual = alvo;
    loop {
        if let Ok(rel) = atual.strip_prefix(raiz) {
            if !rel.as_os_str().is_empty()
                && gitignore.matched_path_or_any_parents(rel, atual.is_dir()).is_ignore()
            {
                return true;
            }
        }
        match atual.parent() {
            Some(p) if p.starts_with(raiz) && p != raiz => atual = p,
            _ => return false,
        }
    }
}

/// Mesma regra do `grep` (#28), e pelo mesmo motivo: o `.gitignore` governa
/// onde a gente entra SOZINHO, nunca o que foi pedido pelo nome.
///
/// Medido neste repo (3,1 GB de `target/`): `glob("**/*.rs")` devolvia 48
/// arquivos em 65ms -- 38 deles artefato de build que ninguem queria. Agora
/// devolve os 10 do codigo. Quem realmente quer o que esta ignorado pede
/// pelo nome: `glob("target/**/*.rs")` continua entrando la, porque ai a
/// escolha e explicita.
fn glob_impl(sandbox: &Sandbox, pattern: &str) -> Result<Array, Box<EvalAltResult>> {
    if pattern.contains("..") {
        return Err(to_err("glob: `..` is not allowed in patterns"));
    }

    let prefixo = prefixo_literal(pattern);
    let inicio = if prefixo.is_empty() {
        sandbox.root.clone()
    } else {
        sandbox.resolve(prefixo).map_err(to_err)?
    };
    if !inicio.exists() {
        return Ok(Array::new());
    }
    let pediu_ignorado = !prefixo.is_empty() && caminho_ignorado(&sandbox.root, &inicio);

    let padrao =
        glob::Pattern::new(pattern).map_err(|e| to_err(format!("glob: invalid pattern: {e}")))?;
    let opcoes = glob::MatchOptions { require_literal_separator: true, ..Default::default() };

    let mut construtor = ignore::WalkBuilder::new(&inicio);
    construtor
        .git_ignore(!pediu_ignorado)
        .git_global(false)
        .git_exclude(!pediu_ignorado)
        .hidden(false);

    let mut out: Vec<String> = Vec::new();
    for entrada in construtor.build().flatten() {
        if entrada.file_name() == ".git" {
            continue;
        }
        let Ok(rel) = entrada.path().strip_prefix(&sandbox.root) else { continue };
        let rel_txt = rel.to_string_lossy().to_string();
        if rel_txt.is_empty() {
            continue;
        }
        if padrao.matches_with(&rel_txt, opcoes) {
            out.push(rel_txt);
        }
    }
    // Walker devolve fora de ordem; resultado de glob precisa ser estavel.
    out.sort();
    Ok(out.into_iter().map(rhai::Dynamic::from).collect())
}

/// Tally de chamadas de primitiva, incrementado nos próprios pontos de
/// registro: o que a telemetria reporta é o que o engine de fato despachou,
/// não um regex adivinhando sobre o fonte (issue #11).
pub type Counter = std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, u64>>>;

pub fn new_counter() -> Counter {
    std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Total de primitivas despachadas, separado do mapa por nome porque a
/// guarda de ociosidade (#18) le este numero a cada operacao de VM: pegar o
/// Mutex e somar o BTreeMap ali custava 27% do tempo de um script
/// computacional (#39).
static TOTAL_CHAMADAS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn total_chamadas() -> u64 {
    TOTAL_CHAMADAS.load(std::sync::atomic::Ordering::Relaxed)
}

fn bump(c: &Counter, nome: &str) {
    TOTAL_CHAMADAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut m) = c.lock() {
        *m.entry(nome.to_string()).or_insert(0) += 1;
    }
}

// ---------------------------------------------------------------------------
// Buraco de stdlib (#14). Cada uma destas saiu de uma falha real
// `Function not found` -- o custo de não existir é a execução inteira
// jogada fora mais um round-trip de LLM pra rediagnosticar.
// ---------------------------------------------------------------------------

fn dyn_to_json(d: &rhai::Dynamic) -> serde_json::Value {
    use serde_json::Value;
    if d.is_unit() {
        return Value::Null;
    }
    if let Some(b) = d.clone().try_cast::<bool>() {
        return Value::Bool(b);
    }
    if let Some(i) = d.clone().try_cast::<i64>() {
        return Value::from(i);
    }
    if let Some(f) = d.clone().try_cast::<f64>() {
        return serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null);
    }
    if let Some(s) = d.clone().try_cast::<String>() {
        return Value::String(s);
    }
    if let Some(a) = d.clone().try_cast::<Array>() {
        return Value::Array(a.iter().map(dyn_to_json).collect());
    }
    if let Some(m) = d.clone().try_cast::<Map>() {
        return Value::Object(m.iter().map(|(k, v)| (k.to_string(), dyn_to_json(v))).collect());
    }
    Value::String(d.to_string())
}

fn json_to_dyn(v: &serde_json::Value) -> rhai::Dynamic {
    use serde_json::Value;
    match v {
        Value::Null => rhai::Dynamic::UNIT,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else {
                n.as_f64().unwrap_or(0.0).into()
            }
        }
        Value::String(s) => s.clone().into(),
        Value::Array(a) => {
            let arr: Array = a.iter().map(json_to_dyn).collect();
            arr.into()
        }
        Value::Object(o) => {
            let mut m = Map::new();
            for (k, val) in o {
                m.insert(k.as_str().into(), json_to_dyn(val));
            }
            m.into()
        }
    }
}

fn path_exists_impl(sandbox: &Sandbox, path: &str) -> Result<bool, Box<EvalAltResult>> {
    let p = sandbox.resolve(path).map_err(to_err)?;
    Ok(p.exists())
}

/// Executa um `Command` com deadline e heartbeat (#18).
///
/// Substitui `Command::output()`, que espera para sempre: era isso que
/// obrigava a regra "suíte de teste fica fora do script" e quebrava todo
/// fluxo de verificação em duas tool-calls. A espera é um poll com backoff
/// exponencial (200µs até 20ms) para não somar latência a comando rápido --
/// as duas pontas dos pipes são drenadas em threads, senão um comando
/// falante encheria o buffer e travaria antes do deadline.
/// Comandos rodando em paralelo neste processo (#45).
static EM_PARALELO: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct GuardaParalelo;

impl Drop for GuardaParalelo {
    fn drop(&mut self) {
        EM_PARALELO.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn exec_com_deadline(
    mut c: Command,
    timeout: u64,
    rotulo: &str,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    // Sem deadline não há o que vigiar: volta ao caminho blocante, que é o
    // mais barato (medido: a vigilância custa ~0,37ms por run_shell).
    if timeout == 0 {
        return c.output();
    }

    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn()?;
    let inicio = Instant::now();
    let limite = Some(Duration::from_secs(timeout));

    // Fase rápida: a maioria dos comandos termina em poucos ms. Enquanto
    // isso não passar, só cedemos a CPU -- sem dormir (a granularidade real
    // do sleep no macOS é ~1ms, mais do que o comando inteiro costuma
    // custar) e sem gastar duas threads de leitura. Medido em três
    // `run_shell("true")`: 8,0ms antes do deadline existir, 12,3ms com poll
    // dormindo e threads sempre, 8,6ms assim.
    // Com N comandos concorrentes seriam N threads girando e disputando CPU
    // com os proprios comandos: sob paralelismo a janela encolhe (#45).
    let janela_rapida = if EM_PARALELO.load(std::sync::atomic::Ordering::Relaxed) > 0 {
        Duration::from_micros(500)
    } else {
        Duration::from_millis(8)
    };
    while inicio.elapsed() < janela_rapida {
        if let Some(status) = child.try_wait()? {
            let mut so = Vec::new();
            let mut se = Vec::new();
            if let Some(s) = child.stdout.as_mut() {
                let _ = s.read_to_end(&mut so);
            }
            if let Some(s) = child.stderr.as_mut() {
                let _ = s.read_to_end(&mut se);
            }
            return Ok(std::process::Output { status, stdout: so, stderr: se });
        }
        std::thread::yield_now();
    }

    // Comando demorado: agora sim vale a pena drenar os pipes em threads --
    // sem isso um comando falante enche o buffer do pipe e trava antes do
    // deadline -- e passar a dormir entre as checagens.
    let mut saida = child.stdout.take();
    let mut erro = child.stderr.take();
    let h_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = saida.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });
    let h_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(s) = erro.as_mut() {
            let _ = s.read_to_end(&mut b);
        }
        b
    });

    let mut proximo_beat = Duration::from_secs(10);
    let mut nap = Duration::from_micros(250);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if let Some(d) = limite {
            if inicio.elapsed() >= d {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("comando excedeu {timeout}s e foi morto: {}", corta(rotulo, 120)),
                ));
            }
        }
        if inicio.elapsed() >= proximo_beat {
            eprintln!(
                "codemode: ainda rodando ({}s): {}",
                inicio.elapsed().as_secs(),
                corta(rotulo, 80)
            );
            proximo_beat += Duration::from_secs(10);
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(20));
    };

    Ok(std::process::Output {
        status,
        stdout: h_out.join().unwrap_or_default(),
        stderr: h_err.join().unwrap_or_default(),
    })
}

fn corta(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

/// `parallel_shell` (#19): N comandos de shell concorrentes, ordem
/// preservada. 49 dos 129 scripts inline medidos tinham laço serial de
/// subprocesso -- somar latência à toa é o que mais empurra script contra o
/// deadline. Closure do Rhai não atravessa thread com o modelo de execução
/// atual, então a forma é uma lista de comandos, não um `parallel(itens,
/// |x| ...)` genérico.
fn parallel_shell_impl(sandbox: &Sandbox, cmds: Array) -> Result<Array, Box<EvalAltResult>> {
    let lista: Vec<String> = cmds.iter().map(|c| c.to_string()).collect();

    // Denylist antes de despachar: uma recusa não pode ficar escondida
    // dentro de uma thread e virar erro capturável.
    for cmd in &lista {
        if let Some(rule) = denylist::check(cmd) {
            return Err(Box::new(EvalAltResult::ErrorTerminated(
                format!("denylist:parallel_shell refused, command matches denylist rule '{rule}'")
                    .into(),
                rhai::Position::NONE,
            )));
        }
    }

    let limite = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).max(1);
    let mut saida = Array::new();
    EM_PARALELO.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _guarda = GuardaParalelo;
    for bloco in lista.chunks(limite) {
        let mut parciais: Vec<Result<Map, String>> = Vec::with_capacity(bloco.len());
        std::thread::scope(|escopo| {
            let handles: Vec<_> = bloco
                .iter()
                .map(|cmd| escopo.spawn(move || run_shell_full_impl(sandbox, cmd, false).map_err(|e| e.to_string())))
                .collect();
            for h in handles {
                parciais.push(h.join().unwrap_or_else(|_| Err("thread do parallel_shell morreu".into())));
            }
        });
        for p in parciais {
            match p {
                Ok(m) => saida.push(m.into()),
                Err(e) => return Err(to_err(e)),
            }
        }
    }
    Ok(saida)
}

/// `replace_all_in_glob` (#3): a troca em lote que o mix real pedia -- 27
/// dos 386 usos medidos eram `edit_file`, quase sempre a mesma edição
/// repetida arquivo a arquivo. Devolve os caminhos tocados, e cada escrita
/// passa pela mesma guarda de encolhimento do `write_file`.
fn replace_all_in_glob_impl(
    sandbox: &Sandbox,
    padrao: &str,
    velho: &str,
    novo: &str,
) -> Result<Array, Box<EvalAltResult>> {
    if velho.is_empty() {
        return Err(to_err("replace_all_in_glob: o texto a substituir não pode ser vazio"));
    }
    let alvos = glob_impl(sandbox, padrao)?;
    let mut tocados = Array::new();
    for alvo in alvos {
        let caminho = alvo.to_string();
        let atual = match read_file_impl(sandbox, &caminho) {
            Ok(c) => c,
            // Binário ou ilegível: não é erro do lote, só não se aplica.
            Err(_) => continue,
        };
        if !atual.contains(velho) {
            continue;
        }
        let novo_conteudo = atual.replace(velho, novo);
        if sandbox.dry {
            eprintln!("codemode: [dry-run] replace_all_in_glob -> {caminho}");
        } else {
            write_file_guarded(sandbox, &caminho, &novo_conteudo)?;
        }
        tocados.push(caminho.into());
    }
    Ok(tocados)
}

/// `Engine::new()` registra o StandardPackage inteiro, e isso custava
/// 0,65-0,89ms em TODA execução -- mais do que muitos scripts inteiros.
/// Aqui entram só os pacotes que um script de codemode usa; ficam de fora o
/// de blob (binário) e o de depuração. O teste
/// `vocabulario_rhai_continua_completo` é o contrato desta escolha (#41).
pub fn nova_engine() -> Engine {
    use rhai::packages::Package;
    let mut e = Engine::new_raw();
    rhai::packages::LanguageCorePackage::new().register_into_engine(&mut e);
    rhai::packages::ArithmeticPackage::new().register_into_engine(&mut e);
    rhai::packages::LogicPackage::new().register_into_engine(&mut e);
    rhai::packages::BasicStringPackage::new().register_into_engine(&mut e);
    rhai::packages::MoreStringPackage::new().register_into_engine(&mut e);
    rhai::packages::BasicIteratorPackage::new().register_into_engine(&mut e);
    rhai::packages::BasicFnPackage::new().register_into_engine(&mut e);
    rhai::packages::BasicArrayPackage::new().register_into_engine(&mut e);
    rhai::packages::BasicMapPackage::new().register_into_engine(&mut e);
    rhai::packages::BasicMathPackage::new().register_into_engine(&mut e);
    rhai::packages::BasicTimePackage::new().register_into_engine(&mut e);
    // `Engine::new()` faz isto por baixo; `new_raw()` não.
    e.on_print(|texto| println!("{texto}"));
    e.on_debug(|texto, origem, pos| match origem {
        Some(o) => eprintln!("{o} @ {pos:?} | {texto}"),
        None => eprintln!("{pos:?} | {texto}"),
    });
    e
}

pub fn register(engine: &mut Engine, sandbox: Sandbox, allow_hosts: Vec<String>, counter: Counter) {
    {
        let allow = allow_hosts;
        let ct = counter.clone();
        engine.register_fn("http_get", move |url: &str| -> Result<Map, Box<EvalAltResult>> {
            bump(&ct, "http_get");
            http_get_impl(&allow, url)
        });
    }

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("read_file", move |path: &str| -> Result<String, Box<EvalAltResult>> {
        bump(&ct, "read_file");
        read_file_impl(&sb, path)
    });

    // `read_file(caminho, #{lines: "120-180"})` -- mesma primitiva, mesma
    // contagem na telemetria: e a MESMA operacao, so que sem pagar o arquivo
    // inteiro para olhar um trecho (#61).
    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn(
        "read_file",
        move |path: &str, opcoes: rhai::Map| -> Result<String, Box<EvalAltResult>> {
            bump(&ct, "read_file");
            let Some(spec) = opcoes.get("lines") else {
                return Err(to_err(
                    "read_file: second argument takes #{lines: \"120-180\"} -- no other key is supported",
                ));
            };
            read_file_faixa(&sb, path, &spec.to_string())
        },
    );

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("write_file", move |path: &str, content: &str| -> Result<(), Box<EvalAltResult>> {
        bump(&ct, "write_file");
        write_file_guarded(&sb, path, content)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("write_file_force", move |path: &str, content: &str| -> Result<(), Box<EvalAltResult>> {
        bump(&ct, "write_file_force");
        write_file_impl(&sb, path, content)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("append_file", move |path: &str, content: &str| -> Result<(), Box<EvalAltResult>> {
        bump(&ct, "append_file");
        append_file_impl(&sb, path, content)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn(
        "edit_file",
        move |path: &str, old: &str, new: &str| -> Result<(), Box<EvalAltResult>> {
        bump(&ct, "edit_file");
            edit_file_impl(&sb, path, old, new)
        },
    );

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("run_shell", move |cmd: &str| -> Result<String, Box<EvalAltResult>> {
        bump(&ct, "run_shell");
        run_shell_impl(&sb, cmd, false)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("run_shell", move |cmd: &str, opts: Map| -> Result<String, Box<EvalAltResult>> {
        bump(&ct, "run_shell");
        run_shell_impl(&sb, cmd, confirm_from_map(&opts))
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("run_shell_full", move |cmd: &str| -> Result<Map, Box<EvalAltResult>> {
        bump(&ct, "run_shell_full");
        run_shell_full_impl(&sb, cmd, false)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("run_shell_full", move |cmd: &str, opts: Map| -> Result<Map, Box<EvalAltResult>> {
        bump(&ct, "run_shell_full");
        run_shell_full_impl(&sb, cmd, confirm_from_map(&opts))
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("run_shell_confirmed", move |cmd: &str| -> Result<String, Box<EvalAltResult>> {
        bump(&ct, "run_shell_confirmed");
        run_shell_impl(&sb, cmd, true)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("grep", move |pattern: &str| -> Result<String, Box<EvalAltResult>> {
        bump(&ct, "grep");
        grep_impl(&sb, pattern, ".")
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("grep", move |pattern: &str, path: &str| -> Result<String, Box<EvalAltResult>> {
        bump(&ct, "grep");
        grep_impl(&sb, pattern, path)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("glob", move |pattern: &str| -> Result<Array, Box<EvalAltResult>> {
        bump(&ct, "glob");
        glob_impl(&sb, pattern)
    });


    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("parallel_shell", move |cmds: Array| -> Result<Array, Box<EvalAltResult>> {
        bump(&ct, "parallel_shell");
        parallel_shell_impl(&sb, cmds)
    });

    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn(
        "replace_all_in_glob",
        move |padrao: &str, velho: &str, novo: &str| -> Result<Array, Box<EvalAltResult>> {
            bump(&ct, "replace_all_in_glob");
            replace_all_in_glob_impl(&sb, padrao, velho, novo)
        },
    );

    // --- stdlib (#14) ---
    engine.register_fn("join", |a: Array, sep: &str| -> String {
        a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(sep)
    });
    engine.register_fn("lines", |s: &str| -> Array {
        s.lines().map(|l| rhai::Dynamic::from(l.to_string())).collect()
    });
    engine.register_fn("trimmed", |s: &str| -> String { s.trim().to_string() });
    engine.register_fn("to_json", |d: rhai::Dynamic| -> String {
        serde_json::to_string(&dyn_to_json(&d)).unwrap_or_else(|_| "null".into())
    });
    engine.register_fn("from_json", |s: &str| -> Result<rhai::Dynamic, Box<EvalAltResult>> {
        match serde_json::from_str::<serde_json::Value>(s) {
            Ok(v) => Ok(json_to_dyn(&v)),
            Err(e) => Err(to_err(format!("from_json: JSON inválido: {e}"))),
        }
    });
    engine.register_fn("basename", |p: &str| -> String {
        std::path::Path::new(p).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    });
    engine.register_fn("dirname", |p: &str| -> String {
        std::path::Path::new(p).parent().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    });
    let sb = sandbox.clone();
    let ct = counter.clone();
    engine.register_fn("path_exists", move |path: &str| -> Result<bool, Box<EvalAltResult>> {
        bump(&ct, "path_exists");
        path_exists_impl(&sb, path)
    });

    // Rhai's own `replace` MUTATES the string in place and returns unit, so
    // `let novo = velho.replace(a, b)` silently binds `()` -- and `() + texto`
    // is just `texto`. That is exactly how a batch script wiped 70 files on
    // 2026-08-19: the read worked, the assembly evaluated to the new block
    // alone, and the write replaced every file with it. Rather than only
    // guarding the write, give scripts the non-mutating form they were
    // reaching for.
    engine.register_fn("replaced", |s: &str, old: &str, new: &str| -> String {
        s.replace(old, new)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> Sandbox {
        Sandbox::new(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// The precise regression check for the bug caught before it shipped:
    /// `git log` with a flag the in-process filter doesn't understand
    /// (`--oneline`) must return `None` from the routing function itself --
    /// not silently produce unfiltered output. `None` here is a contract
    /// with the caller (`run_shell_impl`): it means "fall through to the
    /// rtk-binary tier," never "give up on filtering entirely."
    #[test]
    fn try_in_process_filter_falls_through_on_extra_git_log_flags() {
        let sb = sandbox();
        let words = vec!["git".to_string(), "log".to_string(), "--oneline".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_none());
    }

    #[test]
    fn try_in_process_filter_handles_bare_git_log_diff_status() {
        let sb = sandbox();
        for words in [
            vec!["git".to_string(), "log".to_string()],
            vec!["git".to_string(), "diff".to_string()],
            vec!["git".to_string(), "status".to_string()],
        ] {
            assert!(
                try_in_process_filter(&words, &sb).is_some(),
                "expected {words:?} to be handled in-process"
            );
        }
    }

    #[test]
    fn try_in_process_filter_handles_git_log_with_bare_limit() {
        let sb = sandbox();
        let words = vec!["git".to_string(), "log".to_string(), "-5".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_some());
    }

    #[test]
    fn try_in_process_filter_ignores_unrelated_commands() {
        let sb = sandbox();
        let words = vec!["ls".to_string(), "-la".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_none());
    }

    #[test]
    fn try_in_process_filter_handles_gh_pr_diff_with_extra_args() {
        let sb = sandbox();
        let words = vec![
            "gh".to_string(),
            "pr".to_string(),
            "diff".to_string(),
            "77".to_string(),
            "--repo".to_string(),
            "owner/repo".to_string(),
        ];
        assert!(try_in_process_filter(&words, &sb).is_some());
    }

    #[test]
    fn try_in_process_filter_ignores_other_gh_subcommands() {
        let sb = sandbox();
        let words = vec!["gh".to_string(), "pr".to_string(), "view".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_none());
    }

    #[test]
    fn try_in_process_filter_handles_npx() {
        let sb = sandbox();
        let words = vec!["npx".to_string(), "--version".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_some());
    }

    #[test]
    fn find_and_grep_are_worth_routing_but_not_in_process() {
        // find/grep get the rtk-binary tier (RTK_WORTH_ROUTING), not an
        // in-process filter -- rtk's find/grep are full reimplementations
        // (own traversal/search engine), not simple &str -> String
        // functions like cargo_test/git/gh's filters. This test locks in
        // that split: routed, but via try_in_process_filter returning None
        // so the caller falls through to spawning `rtk`.
        let sb = sandbox();
        assert!(try_in_process_filter(&["find".to_string(), ".".to_string()], &sb).is_none());
        assert!(try_in_process_filter(&["grep".to_string(), "foo".to_string()], &sb).is_none());
        assert!(RTK_WORTH_ROUTING.contains(&"find"));
        assert!(RTK_WORTH_ROUTING.contains(&"grep"));
    }

    /// `pnpm test`/`pnpm run x`/`npm test`/`npm run x` are in-process now
    /// (rtk::filters::pnpm_run) -- but pnpm's OTHER subcommands
    /// (list/install/outdated) must still fall through to the rtk binary,
    /// which has real handlers for exactly those. Same contract as the git
    /// arms: None means "next tier", never "raw".
    #[test]
    fn try_in_process_filter_handles_pnpm_npm_test_and_run() {
        let sb = sandbox();
        // `npm run` with no script name exits non-zero but still produces
        // output -- a valid in-process match either way. Using npm (always
        // installed) rather than pnpm keeps this test machine-portable.
        let words = vec!["npm".to_string(), "run".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_some());
        // Other pnpm subcommands stay on the rtk-binary tier.
        let words = vec!["pnpm".to_string(), "install".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_none());
        let words = vec!["pnpm".to_string(), "list".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_none());
    }

    /// Every make invocation is in-process -- the rtk binary has no make
    /// handler at all (thurionapp/rtk#3), so falling through to it would
    /// pay the spawn tax for a zero-filter passthrough.
    #[test]
    fn try_in_process_filter_handles_make() {
        let sb = sandbox();
        // -qp never runs a real target: prints the database and exits.
        let words = vec!["make".to_string(), "-qp".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_some());
        assert!(RTK_WORTH_ROUTING.contains(&"make"));
    }

    /// The exact re-benchmark regression: `grep -c` (and any -l/-L/-o/-q
    /// shape) is a guaranteed rtk passthrough -- must run raw, not pay the
    /// rtk spawn. Plain content-matching grep still routes.
    #[test]
    fn grep_format_flag_shapes_run_raw_not_routed() {
        let w = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert!(grep_shape_rtk_would_passthrough(&w("grep -c VERSION= b.conf")));
        assert!(grep_shape_rtk_would_passthrough(&w("grep -rl pattern src/")));
        assert!(grep_shape_rtk_would_passthrough(&w("grep --files-with-matches x .")));
        assert!(!grep_shape_rtk_would_passthrough(&w("grep -rn pattern src/")));
        assert!(!grep_shape_rtk_would_passthrough(&w("grep pattern file.txt")));
        // Long flags that merely CONTAIN the letters must not false-positive.
        assert!(!grep_shape_rtk_would_passthrough(&w("grep --color=never pat f")));
        assert!(!grep_shape_rtk_would_passthrough(&w("find . -name x")));
    }

    #[test]
    fn parse_http_host_is_strict() {
        assert_eq!(parse_http_host("https://api.github.com/repos").unwrap(), ("api.github.com".into(), None, true));
        assert_eq!(parse_http_host("http://127.0.0.1:8080/x").unwrap(), ("127.0.0.1".into(), Some(8080), false));
        assert!(parse_http_host("ftp://x.com/").is_err());
        assert!(parse_http_host("file:///etc/passwd").is_err());
        assert!(parse_http_host("https://user@evil.com/").is_err());
        assert!(parse_http_host("https://[::1]:80/").is_err());
        assert!(parse_http_host("https:///path").is_err());
        assert!(parse_http_host("https://h:99999/").is_err());
    }

    #[test]
    fn host_allowed_matches_host_and_optional_port() {
        let allow = vec!["API.GitHub.com".to_string(), "127.0.0.1:8080".to_string()];
        assert!(host_allowed(&allow, "api.github.com", None, true));
        assert!(host_allowed(&allow, "api.github.com", Some(9999), true));
        assert!(host_allowed(&allow, "127.0.0.1", Some(8080), false));
        assert!(!host_allowed(&allow, "127.0.0.1", Some(8081), false));
        assert!(!host_allowed(&allow, "evil.com", None, true));
        assert!(!host_allowed(&[], "api.github.com", None, true));
    }

    #[test]
    fn ls_is_worth_routing_but_not_in_process() {
        // rtk HAS a real ls handler (78.3% avg savings in its own global
        // history) -- rtk-binary tier, same split as find/grep.
        let sb = sandbox();
        assert!(try_in_process_filter(&["ls".to_string(), "-la".to_string()], &sb).is_none());
        assert!(RTK_WORTH_ROUTING.contains(&"ls"));
    }

    #[test]
    fn try_in_process_filter_ignores_bare_npx() {
        // Bare "npx" with no tool name isn't a real invocation -- don't
        // spawn it, fall through like any other non-match.
        let sb = sandbox();
        let words = vec!["npx".to_string()];
        assert!(try_in_process_filter(&words, &sb).is_none());
    }
}
