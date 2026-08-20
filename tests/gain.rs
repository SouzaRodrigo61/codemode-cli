//! Telemetria (#11) e relatório (#12).
//!
//! Todo teste aponta CODEMODE_HOME para um tempdir: telemetria de teste
//! nunca pode contaminar o histórico real do usuário -- é justamente esse
//! histórico que o `codemode gain` existe para reportar.

use assert_cmd::Command;
use std::fs;
use std::path::Path;

fn run_in(home: &Path, dir: &Path, script: &str) -> assert_cmd::assert::Assert {
    let s = dir.join("s.rhai");
    fs::write(&s, script).unwrap();
    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home)
        .arg("run")
        .arg(&s)
        .arg("--workdir")
        .arg(dir)
        .assert()
}

fn linhas(home: &Path) -> Vec<serde_json::Value> {
    let raw = fs::read_to_string(home.join("runs.jsonl")).unwrap_or_default();
    raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn grava_uma_linha_por_execucao_com_contagem_real() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    run_in(
        home.path(),
        dir.path(),
        r#"write_file("a.txt", "x"); let c = read_file("a.txt"); print(c); run_shell("true");"#,
    )
    .success();

    let l = linhas(home.path());
    assert_eq!(l.len(), 1, "uma execução, uma linha");
    let e = &l[0];
    assert_eq!(e["prim_total"], 3, "write_file + read_file + run_shell");
    assert_eq!(e["prims"]["write_file"], 1);
    assert_eq!(e["prims"]["read_file"], 1);
    assert_eq!(e["prims"]["run_shell"], 1);
    assert_eq!(e["exit_code"], 0);
    assert_eq!(e["source"], "file");
    assert!(e["ts"].as_u64().unwrap() > 0);
    // Só metadado: nem fonte, nem conteúdo de arquivo, nem saída.
    let bruto = fs::read_to_string(home.path().join("runs.jsonl")).unwrap();
    assert!(!bruto.contains("write_file(\"a.txt\""), "o fonte não pode ir pro log");
}

#[test]
fn conta_o_que_o_engine_despachou_nao_o_que_o_fonte_parece() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Uma chamada dentro de laço conta 3 vezes; uma em comentário, zero.
    run_in(
        home.path(),
        dir.path(),
        "for i in 0..3 { run_shell(\"true\"); }\n// run_shell(\"nunca\");\n",
    )
    .success();

    let l = linhas(home.path());
    assert_eq!(l[0]["prims"]["run_shell"], 3);
    assert_eq!(l[0]["prim_total"], 3);
}

#[test]
fn grava_tambem_a_execucao_que_falhou() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Falha de runtime (o arquivo não existe), não de pré-voo: o que rodou
    // antes continua contado.
    run_in(home.path(), dir.path(), r#"run_shell("true"); read_file("nao-existe.txt");"#).failure();

    let l = linhas(home.path());
    assert_eq!(l.len(), 1);
    assert_eq!(l[0]["exit_code"], 1, "taxa de erro é um dos números do relatório");
    assert_eq!(l[0]["prims"]["run_shell"], 1, "o que rodou antes da falha continua contado");
}

#[test]
fn falha_de_pre_voo_tambem_entra_no_log_com_zero_primitivas() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    run_in(home.path(), dir.path(), r#"run_shell("true"); nao_existe();"#).failure();

    let l = linhas(home.path());
    assert_eq!(l.len(), 1, "execução barrada no pré-voo ainda conta pra taxa de erro");
    assert_eq!(l[0]["exit_code"], 1);
    assert_eq!(l[0]["prim_total"], 0, "nada rodou: o pré-voo barrou antes");
}

#[test]
fn script_de_biblioteca_e_marcado_como_lib() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".codemode")).unwrap();
    fs::write(dir.path().join(".codemode/verify.rhai"), r#"run_shell("true");"#).unwrap();

    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["run", "verify.rhai", "--workdir"])
        .arg(dir.path())
        .assert()
        .success();

    let l = linhas(home.path());
    assert_eq!(l[0]["source"], "lib");
    assert_eq!(l[0]["name"], "verify.rhai");
}

#[test]
fn falha_de_escrita_do_log_nunca_derruba_a_execucao() {
    let dir = tempfile::tempdir().unwrap();
    // Um arquivo comum no lugar do diretório: create_dir_all falha, e a
    // execução tem que passar assim mesmo.
    let ocupado = dir.path().join("home-ocupado");
    fs::write(&ocupado, "não sou diretório").unwrap();

    let s = dir.path().join("s.rhai");
    fs::write(&s, r#"print("vivo");"#).unwrap();
    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", &ocupado)
        .arg("run")
        .arg(&s)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("vivo"));
}

#[test]
fn telemetria_pode_ser_desligada() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path().join("s.rhai");
    fs::write(&s, r#"run_shell("true");"#).unwrap();
    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .env("CODEMODE_NO_TELEMETRY", "1")
        .arg("run")
        .arg(&s)
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success();

    assert!(!home.path().join("runs.jsonl").exists());
}

#[test]
fn gain_reporta_calls_evitadas_e_buckets() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // 3 primitivas -> 2 calls evitadas; 1 primitiva -> 0, e cai no bucket
    // de desperdício.
    run_in(home.path(), dir.path(), r#"run_shell("true"); run_shell("true"); run_shell("true");"#).success();
    run_in(home.path(), dir.path(), r#"run_shell("true");"#).success();

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("gain")
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(texto.contains("Execuções:"), "{texto}");
    assert!(texto.contains("Tool-calls evitadas:"), "{texto}");
    assert!(texto.contains("desperdício"), "{texto}");

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["gain", "--json"])
        .assert()
        .success();
    let j: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("--json tem que ser JSON válido");
    assert_eq!(j["runs"], 2);
    assert_eq!(j["calls_avoided"], 2);
    assert_eq!(j["buckets"]["1"], 1);
    assert_eq!(j["buckets"]["3+"], 1);
    assert_eq!(j["falhas"], 0);
}

#[test]
fn gain_sem_historico_nao_quebra() {
    let home = tempfile::tempdir().unwrap();
    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("gain")
        .assert()
        .success()
        .stdout(predicates::str::contains("nenhuma execução registrada"));
}

#[test]
fn gain_history_lista_execucoes() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    run_in(home.path(), dir.path(), r#"run_shell("true");"#).success();

    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["gain", "--history"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Últimas"));
}

#[test]
fn linha_corrompida_no_log_nao_quebra_o_relatorio() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join("runs.jsonl"), "{lixo\n{\"ts\":1,\"script\":\"a\",\"source\":\"file\",\"prims\":{\"run_shell\":2},\"prim_total\":2,\"out_bytes\":0,\"exit_code\":0,\"ms\":1,\"workdir\":\"/tmp\"}\n").unwrap();

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["gain", "--json"])
        .assert()
        .success();
    let j: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(j["runs"], 1, "a linha corrompida é descartada, o resto conta");
}
