//! Optional bindings to the `maestri` CLI (spatial workspace for AI
//! agents: teammates, notes, portals). codemode is workspace-agnostic by
//! design -- read_file/write_file/edit_file/run_shell/glob are the only
//! functions every install is guaranteed to have. These functions are
//! always registered into a script's Rhai namespace (unlike an earlier
//! version, they no longer depend on an upfront `maestri --help` probe --
//! see `register`'s doc comment for why), but on a machine without
//! Maestri, calling one produces a clear runtime error ("is `maestri` on
//! PATH?") the first and only time the script actually calls it. A script
//! that never calls a `maestri_*` function pays zero cost either way.
//! These are trusted subcommands of a known binary, not arbitrary shell —
//! so unlike `run_shell` there is no denylist here.
//!
//! v1: each call shells out to the `maestri` binary via
//! `std::process::Command` (one process spawn per call). A named-pipe or
//! socket connection straight to the maestri daemon is the obvious v2
//! upgrade if spawn overhead turns out to matter in practice — not done
//! here because it hasn't been measured as a problem yet.
//!
//! Verified against the real `maestri --help` / `maestri <sub> --help`
//! output on this machine before writing these bindings. Notably:
//! `maestri connect "From" "To"` DOES exist as its own subcommand ("Wire
//! two things together") — `recruit` auto-connects the new teammate,
//! but a separate `connect` command already exists for wiring
//! notes/portals manually. No API is invented here beyond what
//! `--help` showed.
//!
//! `maestri_ask`/`maestri_note_*`/`maestri_portal_*` work from any
//! terminal connected to the workspace. `maestri_recruit` (and
//! `maestri_connect`, best-effort) spawn/wire teammates, which is a
//! Maestro-terminal privilege in maestri's model — running a script
//! that calls them from a non-Maestro terminal is expected to fail with
//! maestri's own error, not a codemode bug. codemode has no way to
//! detect ahead of time whether the current terminal is the Maestro.

use rhai::EvalAltResult;
use std::process::Command;

fn to_err(msg: impl Into<String>) -> Box<EvalAltResult> {
    msg.into().into()
}

fn run_maestri(args: &[&str]) -> Result<String, Box<EvalAltResult>> {
    let output = Command::new("maestri").args(args).output().map_err(|e| {
        to_err(format!(
            "maestri_*: failed to run `maestri {}`: {e} (is `maestri` on PATH?)",
            args.join(" ")
        ))
    })?;
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        return Err(to_err(format!(
            "maestri {} failed (exit {}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            combined.trim()
        )));
    }
    Ok(combined.trim().to_string())
}

fn maestri_ask_impl(agent: &str, prompt: &str) -> Result<String, Box<EvalAltResult>> {
    run_maestri(&["ask", agent, prompt])
}

fn maestri_note_read_impl(name: &str) -> Result<String, Box<EvalAltResult>> {
    run_maestri(&["note", "read", name])
}

fn maestri_note_write_impl(name: &str, content: &str) -> Result<String, Box<EvalAltResult>> {
    run_maestri(&["note", "write", name, content])
}

fn maestri_note_create_impl(content: &str) -> Result<String, Box<EvalAltResult>> {
    // Without --name, maestri derives the note's name from its first
    // line and RENAMES it on every subsequent `write` that changes that
    // first line (confirmed against the real binary: a create + write +
    // read round trip using the derived name broke because `write`
    // silently renamed the note out from under it). To keep the name
    // `maestri_note_write`/`maestri_note_read` receive back from this
    // call stable across later writes, pin an explicit name with
    // `--name` instead of relying on the auto-derived one.
    let name = format!("codemode-note-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0));
    run_maestri(&["note", "create", content, "--name", &name])?;
    Ok(name)
}

fn maestri_portal_create_impl(url: &str, name: &str) -> Result<String, Box<EvalAltResult>> {
    run_maestri(&["portal", "create", url, name])
}

fn maestri_recruit_impl(name: &str, role: &str, path: &str) -> Result<String, Box<EvalAltResult>> {
    // `recruit` is Maestro-only in maestri's model (spawning/dismissing
    // teammates is a privilege of the Maestro terminal). If this script
    // isn't running from a Maestro terminal, maestri itself will refuse
    // the call and we surface its stderr as-is — codemode has no way to
    // detect "am I a Maestro terminal" ahead of time, so this is not
    // treated as a codemode bug when it happens.
    run_maestri(&["recruit", name, "--role", role, "--dir", path])
}

fn maestri_connect_impl(a: &str, b: &str) -> Result<String, Box<EvalAltResult>> {
    // Same Maestro-only caveat as `recruit`/`dismiss`: unconfirmed
    // whether `connect` enforces it as strictly, treat as best-effort
    // and let maestri's own error surface if it refuses.
    run_maestri(&["connect", a, b])
}

fn maestri_note_delete_impl(name: &str) -> Result<String, Box<EvalAltResult>> {
    run_maestri(&["note", "delete", name])
}

/// codemode is not a Maestri-specific tool -- these bindings are one
/// optional extension among possibly many (a future Cursor/Devin/
/// whatever-specific module would follow the same pattern). Whether
/// `maestri` is actually on PATH is checked lazily, per call, by each
/// `run_maestri` invocation itself -- NOT by an upfront probe here.
///
/// v1 of this file gated `register()` behind an eager `maestri --help`
/// probe on every single `codemode run`, regardless of whether the script
/// called any `maestri_*` function. Measured directly: that probe alone
/// costs ~4.3ms -- more than the rest of a typical codemode invocation
/// combined (~3.3ms: binary startup + file I/O), roughly doubling
/// wall-clock for the common case of a script that never touches Maestri
/// at all. Same class of mistake as the run_shell/rtk routing regression
/// found the same day (an availability probe that costs as much as the
/// thing it's guarding) -- fixed the same way: attempt the real
/// `maestri` subprocess spawn only when a script actually calls one of
/// these functions, and let `Command::output()`'s own `NotFound` error
/// surface a clear message (see `run_maestri`'s `map_err`) instead of
/// paying to check ahead of time on every run.
pub fn register(engine: &mut rhai::Engine) {
    engine.register_fn("maestri_ask", maestri_ask_impl);
    engine.register_fn("maestri_note_read", maestri_note_read_impl);
    engine.register_fn("maestri_note_write", maestri_note_write_impl);
    engine.register_fn("maestri_note_create", maestri_note_create_impl);
    engine.register_fn("maestri_note_delete", maestri_note_delete_impl);
    engine.register_fn("maestri_portal_create", maestri_portal_create_impl);
    engine.register_fn("maestri_recruit", maestri_recruit_impl);
    engine.register_fn("maestri_connect", maestri_connect_impl);
}
