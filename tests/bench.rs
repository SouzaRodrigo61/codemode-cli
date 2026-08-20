use assert_cmd::Command;
use predicates::str::contains;
use std::fs;

fn cmd() -> Command {
    let mut c = Command::cargo_bin("codemode").unwrap();
    // Teste nunca escreve no histórico real: é ele que `codemode gain`
    // reporta, e execução de teste não é uso.
    c.env("CODEMODE_NO_TELEMETRY", "1");
    c
}

#[test]
fn bench_runs_and_reports_codemode_only() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"write_file("out.txt", "hi");"#).unwrap();

    cmd()
        .arg("bench")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--n")
        .arg("3")
        .assert()
        .success()
        .stdout(contains("codemode"))
        .stdout(contains("median="));
}

#[test]
fn bench_compare_reports_ratio() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"write_file("out.txt", "hi");"#).unwrap();

    cmd()
        .arg("bench")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--compare")
        .arg("true")
        .arg("--n")
        .arg("3")
        .assert()
        .success()
        .stdout(contains("compare"))
        .stdout(contains("faster"));
}

#[test]
fn bench_reset_cmd_runs_before_each_iteration() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    let counter = dir.path().join("counter.txt");
    fs::write(&counter, "0").unwrap();
    // Script that fails unless counter.txt reads "reset" -- proves reset-cmd
    // ran before this iteration, not just once at the start.
    fs::write(
        &script,
        r#"
        let v = read_file("counter.txt");
        if v != "reset" { throw "counter was not reset: " + v; }
        write_file("counter.txt", "dirty");
        "#,
    )
    .unwrap();

    cmd()
        .arg("bench")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--reset-cmd")
        .arg("printf reset > counter.txt")
        .arg("--n")
        .arg("3")
        .assert()
        .success();
}

#[test]
fn bench_fails_clearly_when_measured_script_errors() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"throw "boom";"#).unwrap();

    cmd()
        .arg("bench")
        .arg(&script)
        .arg("--workdir")
        .arg(dir.path())
        .arg("--n")
        .arg("2")
        .assert()
        .failure();
}

#[test]
fn bench_nao_grava_telemetria() {
    // Iteração de benchmark não é uso. Sem isto, medir a performance
    // envenena o número que o `codemode gain` reporta (#36).
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.rhai");
    std::fs::write(&script, "let x = 1;").unwrap();

    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .env_remove("CODEMODE_NO_TELEMETRY")
        .args(["bench", script.to_str().unwrap(), "--workdir"])
        .arg(dir.path())
        .args(["--n", "3"])
        .assert()
        .success();

    assert!(
        !home.path().join("runs.jsonl").exists(),
        "bench não pode gravar execução no histórico que o gain reporta"
    );
}
