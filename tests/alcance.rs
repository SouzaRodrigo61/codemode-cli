//! Alcance: deadline por comando, vm-idle, multi-root e paralelismo
//! (#18, #4, #19).

use assert_cmd::Command;
use std::fs;
use std::path::Path;
use std::time::Instant;

fn cmd() -> Command {
    let mut c = Command::cargo_bin("codemode").unwrap();
    c.env("CODEMODE_NO_TELEMETRY", "1");
    c
}

fn escreve(dir: &Path, src: &str) -> std::path::PathBuf {
    let p = dir.join("s.rhai");
    fs::write(&p, src).unwrap();
    p
}

#[test]
fn comando_que_passa_do_cmd_timeout_e_morto_e_o_script_segue() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(
        dir.path(),
        r#"
try {
    run_shell("sleep 9");
    print("nao devia chegar aqui");
} catch (e) {
    print("morto=" + e);
}
print("segui em frente");
"#,
    );
    let inicio = Instant::now();
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--cmd-timeout", "1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("morto="))
        .stdout(predicates::str::contains("segui em frente"));
    assert!(inicio.elapsed().as_secs() < 6, "devia morrer em ~1s, não esperar os 9");
}

#[test]
fn edita_roda_comando_longo_e_decide_numa_chamada_so() {
    // O caso de uso que o cap global de 120s proibia: editar, verificar e
    // reverter no fracasso, tudo dentro de um script.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("codigo.txt"), "versao boa").unwrap();
    let s = escreve(
        dir.path(),
        r#"
let antes = read_file("codigo.txt");
write_file("codigo.txt", "versao nova");
let verificacao = run_shell_full("sh -c 'sleep 2; exit 1'");
if !verificacao.success {
    write_file("codigo.txt", antes);
    print("revertido");
} else {
    print("mantido");
}
"#,
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--timeout", "0"])
        .assert()
        .success()
        .stdout(predicates::str::contains("revertido"));
    assert_eq!(fs::read_to_string(dir.path().join("codigo.txt")).unwrap(), "versao boa");
}

#[test]
fn laco_puro_de_vm_morre_pela_guarda_de_ociosidade_mesmo_sem_timeout_global() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "let i = 0; loop { i += 1; }");
    let inicio = Instant::now();
    let out = cmd()
        .args([
            "run",
            s.to_str().unwrap(),
            "--workdir",
            dir.path().to_str().unwrap(),
            "--timeout",
            "0",
            "--vm-idle",
            "2",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(124), "abort por limite tem código 124");
    assert!(inicio.elapsed().as_secs() < 15, "a guarda de ociosidade tem que agir sozinha");
}

#[test]
fn timeout_global_continua_valendo_quando_pedido() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), r#"run_shell("sleep 9");"#);
    let out = cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--timeout", "2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(124));
}

#[test]
fn extra_root_le_e_escreve_no_outro_repo() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let fora = tempfile::tempdir().unwrap();
    fs::write(b.path().join("pacote.txt"), "conteudo do outro repo").unwrap();

    let s = escreve(
        a.path(),
        &format!(
            r#"
let c = read_file("{outro}/pacote.txt");
print("li=" + c);
write_file("{outro}/gerado.txt", "escrito de outro root");
try {{
    read_file("{proibido}/segredo.txt");
    print("NAO DEVIA LER");
}} catch (e) {{
    print("bloqueado");
}}
"#,
            outro = b.path().display(),
            proibido = fora.path().display()
        ),
    );
    fs::write(fora.path().join("segredo.txt"), "nao").unwrap();

    cmd()
        .args([
            "run",
            s.to_str().unwrap(),
            "--workdir",
            a.path().to_str().unwrap(),
            "--extra-root",
            b.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicates::str::contains("li=conteudo do outro repo"))
        .stdout(predicates::str::contains("bloqueado"));

    assert_eq!(fs::read_to_string(b.path().join("gerado.txt")).unwrap(), "escrito de outro root");
}

#[test]
fn extra_root_inexistente_e_erro_claro() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "print(1);");
    cmd()
        .args([
            "run",
            s.to_str().unwrap(),
            "--workdir",
            dir.path().to_str().unwrap(),
            "--extra-root",
            "/caminho/que/nao/existe",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("não existe"));
}

#[test]
fn parallel_shell_preserva_ordem_e_reporta_falha_por_item() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(
        dir.path(),
        r#"
let r = parallel_shell(["echo um", "sh -c 'exit 7'", "echo tres"]);
print("n=" + r.len());
print("0=" + trimmed(r[0].stdout) + " ok=" + r[0].success);
print("1=" + r[1].exit_code + " ok=" + r[1].success);
print("2=" + trimmed(r[2].stdout));
"#,
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("n=3"))
        .stdout(predicates::str::contains("0=um ok=true"))
        .stdout(predicates::str::contains("1=7 ok=false"))
        .stdout(predicates::str::contains("2=tres"));
}

#[test]
fn parallel_shell_roda_de_fato_em_paralelo() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(
        dir.path(),
        r#"let r = parallel_shell(["sleep 1", "sleep 1", "sleep 1", "sleep 1"]); print("n=" + r.len());"#,
    );
    let inicio = Instant::now();
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("n=4"));
    // Serial seriam 4s. Margem larga de propósito: o teste é sobre não ser
    // serial, não sobre o número exato.
    assert!(inicio.elapsed().as_secs() < 3, "4 sleeps de 1s em paralelo não podem levar 4s");
}

#[test]
fn parallel_shell_recusa_comando_da_denylist_antes_de_despachar() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(
        dir.path(),
        r#"
try {
    parallel_shell(["echo ok", "rm -rf /"]);
    print("NAO DEVIA PASSAR");
} catch (e) {
    print("capturado indevidamente");
}
"#,
    );
    // A recusa é ErrorTerminated: nem o try/catch do script pode engolir.
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("denylist"));
}
