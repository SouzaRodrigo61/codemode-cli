use assert_cmd::Command;
use std::fs;
use std::time::Duration;

fn cmd() -> Command {
    Command::cargo_bin("codemode").unwrap()
}

#[test]
fn path_traversal_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"read_file("../../etc/passwd");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure();
}

#[test]
fn edit_file_requires_unique_match() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f.txt"), "foo foo").unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"edit_file("f.txt", "foo", "bar");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("not unique"));
}

#[test]
fn edit_file_requires_match_present() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f.txt"), "hello").unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"edit_file("f.txt", "nope", "bar");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("not found"));
}

#[test]
fn edit_file_replaces_unique_match() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f.txt"), "hello world").unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"edit_file("f.txt", "world", "there");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success();

    let content = fs::read_to_string(dir.path().join("f.txt")).unwrap();
    assert_eq!(content, "hello there");
}

fn rtk_available() -> bool {
    std::process::Command::new("rtk")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A plain single command (no pipes/redirects/chains) that RTK recognizes
/// gets routed for the same output trimming the top-level Bash tool already
/// gets -- otherwise run_shell silently bypasses RTK entirely. `git status`
/// bare now goes through the in-process `rtk::filters::git_status` path
/// (doesn't even need `rtk` on PATH); this test doesn't distinguish that
/// from the rtk-binary tier, just proves *some* form of routing succeeds,
/// using this repo's own checkout rather than mocking anything.
#[test]
fn run_shell_routes_plain_recognized_command_through_rtk() {
    let repo_root = env!("CARGO_MANIFEST_DIR");
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print(run_shell("git status"));"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(repo_root)
        .assert()
        .success();
}

/// `git log`/`git diff`/`git status` bare all route through the in-process
/// rtk::filters::git_* path (see try_in_process_filter) -- doesn't need
/// `rtk` on PATH at all, since it's a library call now, not a spawn.
#[test]
fn run_shell_git_bare_subcommands_use_in_process_filters() {
    let repo_root = env!("CARGO_MANIFEST_DIR");
    for sub in ["git log", "git diff", "git status"] {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("s.rhai");
        fs::write(&script, format!(r#"print(run_shell("{sub}"));"#)).unwrap();

        cmd()
            .arg("run")
            .arg(&script)
            .arg("--workdir")
            .arg(repo_root)
            .assert()
            .success();
    }
}

/// `git log` with a flag the in-process filter doesn't cover (`--oneline`)
/// must still succeed and return real content -- covered precisely by the
/// unit test `try_in_process_filter_falls_through_on_extra_git_log_flags`
/// in `src/primitives.rs` (which asserts the routing function itself
/// returns `None`, so the caller falls through to the rtk-binary tier
/// rather than unfiltered raw `sh -c`). This is just the end-to-end smoke
/// test that the whole path still works.
#[test]
fn run_shell_git_log_with_extra_flags_still_works() {
    let repo_root = env!("CARGO_MANIFEST_DIR");
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print(run_shell("git log --oneline -5"));"#).unwrap();

    cmd().arg("run").arg(&script).arg("--workdir").arg(repo_root).assert().success();
}

/// A small/fast command whose first word isn't in the "worth routing"
/// allowlist (see primitives.rs) must run raw, not through rtk -- routing
/// *every* plain command was tried first and measured to be a net latency
/// loss for commands this cheap (rtk's own startup alone costs more than
/// running them raw). `echo` producing its own literal output, unaltered,
/// is the simplest proof it went through plain `sh -c`.
#[test]
fn run_shell_does_not_route_small_command_not_worth_it() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print(run_shell("echo not-worth-routing"));"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicates::str::diff("not-worth-routing\n\n"));
}

/// A command with shell syntax (here: a pipe) must NOT be split into argv
/// and handed to `rtk` -- `rtk git status | wc -l` as literal argv would
/// mean something completely different from "run git status, pipe to wc".
/// This has to keep going through `sh -c` regardless of whether rtk is on
/// PATH.
#[test]
fn run_shell_with_pipe_bypasses_rtk_routing() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print(run_shell("echo hello | wc -l"));"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("1"));
}

#[test]
fn dangerous_shell_command_refused_without_confirm() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"run_shell("rm -rf somedir");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("refused"));
}

#[test]
fn dangerous_shell_command_runs_with_confirm() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("victim")).unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"run_shell("rm -rf victim", #{confirm: true});"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success();

    assert!(!dir.path().join("victim").exists());
}

#[test]
fn timeout_kills_infinite_loop() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"loop { }"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--timeout")
        .arg("2")
        .timeout(Duration::from_secs(15))
        .assert()
        .code(124);
}

#[test]
fn output_is_truncated_at_cap() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
        let i = 0;
        while i < 2000 {
            print("0123456789");
            i += 1;
        }
        "#,
    )
    .unwrap();

    let assert = cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--max-output")
        .arg("100")
        .assert()
        .success();

    let output = assert.get_output();
    assert!(output.stdout.len() <= 200);
    assert!(String::from_utf8_lossy(&output.stderr).contains("truncated"));
}

#[test]
fn cannot_read_outside_workdir_via_symlink() {
    let outside = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let script = dir.path().join("s.rhai");
        fs::write(&script, r#"read_file("escape/secret.txt");"#).unwrap();

        cmd()
            .arg("run")
            .arg(&script)
            .arg("--workdir")
            .arg(dir.path())
            .assert()
            .failure();
    }
}

#[test]
fn write_read_roundtrip_and_stdin_script() {
    let dir = tempfile::tempdir().unwrap();
    let script = r#"
        write_file("out.txt", "hello codemode");
        print(read_file("out.txt"));
    "#;

    cmd()
        .arg("run")
        .arg("-")
        .arg("--workdir")
        .arg(dir.path())
        .write_stdin(script)
        .assert()
        .success()
        .stdout(predicates::str::contains("hello codemode"));
}

/// The DeepSeek Harness lesson (their discussion #532): a policy denial
/// surfaced as a catchable error lets a model-written script swallow the
/// refusal and hammer the denylist in a loop inside one Bash call. Our
/// denylist hit is ErrorTerminated -- Rhai's try/catch cannot intercept
/// it, so the script dies on the first refusal, catch block never runs.
#[test]
fn denylist_refusal_cannot_be_caught_by_script() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
        try {
            run_shell("rm -rf somedir");
            print("unreachable-ran");
        } catch (e) {
            print("swallowed-the-refusal");
        }
        print("script-continued");
        "#,
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("denylist rule"))
        .stdout(predicates::boolean::PredicateBooleanExt::not(
            predicates::str::contains("swallowed-the-refusal"),
        ))
        .stdout(predicates::boolean::PredicateBooleanExt::not(
            predicates::str::contains("script-continued"),
        ));
}

/// A script written with JS muscle memory gets a targeted hint next to the
/// error instead of just "Variable not found" -- one hint line saves the
/// calling model a whole re-diagnosis round-trip.
#[test]
fn script_error_includes_foreign_idiom_hint() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, "console.log(\"oi\");\n").unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("use print"));
}

/// dsh discussion #2482's bug class: a sandbox shim that makes stat/read of
/// a missing path "succeed" silently breaks every existence check. Pin that
/// read_file on a missing path inside the workdir errors explicitly and
/// names both the function and the path.
#[test]
fn read_file_missing_path_errors_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"read_file("nope.txt");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("read_file"))
        .stderr(predicates::str::contains("nope.txt"));
}

/// Issue #6: run_shell_full returns a typed map the script can branch on
/// -- separate stdout/stderr streams, integer exit_code, boolean success
/// -- instead of scraping a merged prose string.
#[test]
fn run_shell_full_returns_typed_map_with_separate_streams() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
        let r = run_shell_full("sh -c 'echo saida; echo erro 1>&2; exit 3'");
        print("code=" + r.exit_code);
        print("ok=" + r.success);
        let o = r.stdout; o.trim();
        let e = r.stderr; e.trim();
        print("out=" + o);
        print("err=" + e);
        "#,
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("code=3"))
        .stdout(predicates::str::contains("ok=false"))
        .stdout(predicates::str::contains("out=saida"))
        .stdout(predicates::str::contains("err=erro"));
}

/// run_shell_full shares the denylist gate, and the refusal is just as
/// uncatchable as run_shell's.
#[test]
fn run_shell_full_denylist_refusal_uncatchable() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
        try { run_shell_full("rm -rf x"); } catch (e) { print("swallowed"); }
        print("continued");
        "#,
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("denylist rule"))
        .stdout(predicates::boolean::PredicateBooleanExt::not(
            predicates::str::contains("swallowed"),
        ));
}

/// Issue #7: past --max-output the FULL stream spills to a temp file and
/// the truncation notice names the file plus a tail preview -- never a
/// shorter string disguised as the whole output. TMPDIR is overridden so
/// the test can find and verify the spill file byte count.
#[test]
fn truncated_output_spills_full_stream_with_tail_preview() {
    let dir = tempfile::tempdir().unwrap();
    let spill_dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
        let i = 0;
        while i < 500 {
            print("linha-" + i);
            i += 1;
        }
        "#,
    )
    .unwrap();

    let assert = cmd()
        .env("TMPDIR", spill_dir.path())
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--max-output")
        .arg("200")
        .assert()
        .success()
        .stderr(predicates::str::contains("full output:"))
        .stderr(predicates::str::contains("linha-499"));

    let _ = assert;
    let spill = fs::read_dir(spill_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with("codemode-spill-"))
        .expect("spill file should exist");
    let content = fs::read_to_string(spill.path()).unwrap();
    assert!(content.contains("linha-0\n"), "spill must start at byte zero");
    assert!(content.contains("linha-499"), "spill must hold the full stream");
    assert!(content.len() > 200, "spill must exceed the cap");
}

/// Issue #8: http_get is default-closed -- no --allow-host, no network --
/// and the refusal is the same uncatchable terminator as a denylist hit,
/// so a script can't probe hosts inside try/catch.
#[test]
fn http_get_refused_by_default_and_uncatchable() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
        try { http_get("https://example.com/"); } catch (e) { print("swallowed"); }
        print("continued");
        "#,
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("allow-host"))
        .stdout(predicates::boolean::PredicateBooleanExt::not(
            predicates::str::contains("swallowed"),
        ));
}

/// http_get against a real local HTTP server (no mocks -- an actual
/// TcpListener in the test), allowed via --allow-host, returns the typed
/// map with status/body/success.
#[test]
fn http_get_fetches_allowed_host_with_typed_result() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nola-codemode",
            );
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        format!(
            r#"
            let r = http_get("http://127.0.0.1:{port}/");
            print("status=" + r.status);
            print("ok=" + r.success);
            print("body=" + r.body);
            "#
        ),
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--allow-host")
        .arg("127.0.0.1")
        .assert()
        .success()
        .stdout(predicates::str::contains("status=200"))
        .stdout(predicates::str::contains("ok=true"))
        .stdout(predicates::str::contains("body=ola-codemode"));

    server.join().unwrap();
}

/// Issue #9: a bare script name not found as given also resolves against
/// <workdir>/.codemode/ -- the repo's versioned script library is directly
/// runnable without path-deriving.
#[test]
fn bare_script_name_resolves_from_codemode_library_dir() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".codemode")).unwrap();
    fs::write(
        dir.path().join(".codemode/biblioteca.rhai"),
        r#"print("veio-da-biblioteca");"#,
    )
    .unwrap();

    cmd()
        .current_dir(dir.path())
        .arg("run")
        .arg("biblioteca.rhai")
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("veio-da-biblioteca"));

    // An explicit path that doesn't exist must NOT silently fall back.
    cmd()
        .current_dir(dir.path())
        .arg("run")
        .arg("./nao-existe/biblioteca.rhai")
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("failed to read script"));
}

/// --arg values reach the script as the ARGS constant, in order -- what
/// makes a .codemode/ library script reusable instead of edited per run.
#[test]
fn script_args_reach_script_as_args_constant() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print("n=" + ARGS.len + " a=" + ARGS[0] + " b=" + ARGS[1]);"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--arg")
        .arg("77")
        .arg("--arg")
        .arg("owner/repo")
        .assert()
        .success()
        .stdout(predicates::str::contains("n=2 a=77 b=owner/repo"));
}

// ---------------------------------------------------------------------
// Regression: the 2026-08-19 batch that zeroed 70 markdown files.
// The shape was `glob()` -> `read_file()` -> `write_file(f, atual + bloco)`.
// Three separate things had to hold for that to be safe, so each gets a
// test: glob must return paths read_file accepts, read_file must never
// answer "" for a file it couldn't read, and write_file must refuse a
// replacement that erases the file.
// ---------------------------------------------------------------------

/// The exact loop, on files with real content: every original must
/// survive, with the block appended -- not replaced by it.
#[test]
fn append_loop_over_globbed_files_preserves_content() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("docs")).unwrap();
    for i in 0..3 {
        fs::write(
            dir.path().join("docs").join(format!("f{i}.md")),
            format!("conteudo original {i}\nlinha dois\n"),
        )
        .unwrap();
    }
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
let bloco = "\n<!-- marcador -->\nBLOCO\n";
for f in glob("docs/*.md") {
    let atual = read_file(f);
    if atual.contains("marcador") { } else { write_file(f, atual + bloco); }
}
"#,
    )
    .unwrap();

    cmd().arg("run").arg(&script).arg("--workdir").arg(dir.path()).assert().success();

    for i in 0..3 {
        let got = fs::read_to_string(dir.path().join("docs").join(format!("f{i}.md"))).unwrap();
        assert!(got.starts_with(&format!("conteudo original {i}")), "original erased: {got:?}");
        assert!(got.contains("BLOCO"), "block not appended: {got:?}");
    }
}

/// `glob` must hand back paths the sibling primitives accept. Reached
/// through a symlinked workdir, which is the everyday case on macOS
/// (`/tmp` -> `/private/tmp`) and used to make `glob` return raw absolute
/// paths that `read_file` then refused as "outside sandbox".
#[cfg(unix)]
#[test]
fn glob_results_are_readable_through_a_symlinked_workdir() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("a.md"), "ORIGINAL\n").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"for f in glob("*.md") { print(f + ":" + read_file(f).len()); }"#,
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(&link)
        .assert()
        .success()
        .stdout(predicates::str::contains("a.md:9"));
}

/// read_file never answers with "" for a file it could not read -- the
/// silence is what turned a script bug into data loss.
#[test]
fn read_file_errors_instead_of_returning_empty() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print("[" + read_file("nope.md") + "]");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("does not exist"));
}

/// A write that would replace a file with a fraction of its content is
/// refused, and the file on disk is untouched.
#[test]
fn write_file_refuses_to_wipe_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("f.md");
    fs::write(&target, "conteudo original bem mais longo que o bloco\n").unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"write_file("f.md", "BLOCO\n");"#).unwrap();

    cmd()
        .arg("run")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .failure()
        .stderr(predicates::str::contains("recusado"));

    assert_eq!(fs::read_to_string(&target).unwrap(), "conteudo original bem mais longo que o bloco\n");
}

/// ...but a deliberate replacement still has a way through.
#[test]
fn write_file_force_replaces_anyway() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("f.md");
    fs::write(&target, "conteudo original bem mais longo que o bloco\n").unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"write_file_force("f.md", "BLOCO\n");"#).unwrap();

    cmd().arg("run").arg(&script).arg("--workdir").arg(dir.path()).assert().success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "BLOCO\n");
}

/// append_file is the primitive that makes the read+write dance
/// unnecessary in the first place.
#[test]
fn append_file_adds_without_reading() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("f.md");
    fs::write(&target, "ORIGINAL\n").unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"append_file("f.md", "BLOCO\n"); append_file("novo.md", "X\n");"#).unwrap();

    cmd().arg("run").arg(&script).arg("--workdir").arg(dir.path()).assert().success();
    assert_eq!(fs::read_to_string(&target).unwrap(), "ORIGINAL\nBLOCO\n");
    assert_eq!(fs::read_to_string(dir.path().join("novo.md")).unwrap(), "X\n");
}
