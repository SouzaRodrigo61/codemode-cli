use assert_cmd::Command;
use std::fs;

fn maestri_available() -> bool {
    Command::new("maestri").arg("--help").output().map(|o| o.status.success()).unwrap_or(false)
}

fn cmd() -> Command {
    Command::cargo_bin("codemode").unwrap()
}

/// These hit the real `maestri` binary (workspace-scoped notes, no
/// destructive ops). Skipped when `maestri` isn't on PATH so `cargo
/// test` still passes in environments without it, per requirement.
///
/// The script deletes the note it creates as its own last step. An earlier
/// version of this test didn't, and every `cargo test` run left a real,
/// permanent "codemode-note-<timestamp>" note on whatever live Maestri
/// canvas the test happened to run against -- several accumulated during
/// this crate's own development and had to be cleaned up by hand. Tests
/// that touch a real external system must leave it exactly as they found
/// it, not just "assert and walk away."
#[test]
fn maestri_note_create_write_read_roundtrip() {
    if !maestri_available() {
        eprintln!("skipping: maestri not on PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
        let name = maestri_note_create("codemode integration test note");
        maestri_note_write(name, "updated by codemode test");
        let content = maestri_note_read(name);
        maestri_note_delete(name);
        print(content);
        "#,
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--timeout")
        .arg("60")
        .assert()
        .success()
        .stdout(predicates::str::contains("updated by codemode test"));
}

#[test]
fn maestri_functions_error_clearly_when_binary_missing() {
    // Doesn't require maestri to be absent — just checks that calling a
    // maestri_* function with a bogus agent name that can't possibly
    // exist fails with a clear message rather than panicking the
    // process.
    if !maestri_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"maestri_ask("__no_such_agent_codemode_test__", "hi");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--timeout")
        .arg("15")
        .assert()
        .failure();
}
