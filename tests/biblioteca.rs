//! Biblioteca (#20), guarda de trivialidade (#21), --json (#2),
//! replace_all_in_glob (#3) e a folha de armadilhas (#22).

use assert_cmd::Command;
use std::fs;
use std::path::Path;

fn cmd(home: &Path) -> Command {
    let mut c = Command::cargo_bin("codemode").unwrap();
    c.env("CODEMODE_HOME", home);
    c
}

fn escreve(dir: &Path, src: &str) -> std::path::PathBuf {
    let p = dir.join("s.rhai");
    fs::write(&p, src).unwrap();
    p
}

#[test]
fn save_promove_o_ultimo_script_e_list_mostra_o_catalogo() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();

    // Script de stdin: o fonte é guardado, e é este o caso que o `save` sem
    // --from existe para atender (script inline virando ativo do repo).
    cmd(home.path())
        .args(["run", "-", "--workdir", dir.path().to_str().unwrap()])
        .write_stdin("run_shell(\"true\");\nrun_shell(\"true\");\nprint(\"rodei\");\n")
        .assert()
        .success();

    cmd(home.path())
        .args(["save", "verifica", "--workdir", dir.path().to_str().unwrap(), "--desc", "roda a verificação"])
        .assert()
        .success()
        .stdout(predicates::str::contains("verifica.rhai"));

    let salvo = fs::read_to_string(dir.path().join(".codemode/verifica.rhai")).unwrap();
    assert!(salvo.starts_with("// desc: roda a verificação"));
    assert!(salvo.contains("print(\"rodei\")"));

    // Roda pelo nome puro: passa a contar como origem "lib".
    cmd(home.path())
        .args(["run", "verifica.rhai", "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success();

    cmd(home.path())
        .args(["list", "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("verifica.rhai"))
        .stdout(predicates::str::contains("roda a verificação"))
        .stdout(predicates::str::contains("1x"));
}

#[test]
fn script_de_arquivo_nao_paga_copia_do_fonte() {
    // Copiar para last.rhai custava 0,28ms em toda execução, e para script
    // que já está em disco não servia para nada (#42).
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "run_shell(\"true\"); run_shell(\"true\");");
    cmd(home.path())
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success();
    assert!(!home.path().join("last.rhai").exists(), "script de arquivo não copia o fonte");

    // E o `save` diz o que fazer em vez de falhar sem explicação.
    cmd(home.path())
        .args(["save", "x", "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--from"));

    cmd(home.path())
        .args(["save", "x", "--workdir", dir.path().to_str().unwrap(), "--from", s.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn save_nao_sobrescreve_sem_force() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".codemode")).unwrap();
    fs::write(dir.path().join(".codemode/x.rhai"), "// antigo\n").unwrap();
    let origem = dir.path().join("novo.rhai");
    fs::write(&origem, "// novo\n").unwrap();

    cmd(home.path())
        .args(["save", "x", "--workdir", dir.path().to_str().unwrap(), "--from", origem.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("já existe"));

    cmd(home.path())
        .args([
            "save", "x", "--workdir", dir.path().to_str().unwrap(), "--from", origem.to_str().unwrap(), "--force",
        ])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(dir.path().join(".codemode/x.rhai")).unwrap(), "// novo\n");
}

#[test]
fn list_sem_biblioteca_orienta_em_vez_de_falhar() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    cmd(home.path())
        .args(["list", "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("codemode save"));
}

#[test]
fn script_de_uma_primitiva_avisa_e_com_strict_recusa() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "run_shell(\"echo oi\");");

    cmd(home.path())
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicates::str::contains("Bash direto sai mais barato"))
        .stderr(predicates::str::contains("echo oi"));

    let out = cmd(home.path())
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--strict"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).is_empty(), "--strict não executa");
}

#[test]
fn laco_escapa_da_guarda_de_trivialidade() {
    // Uma chamada no fonte pode ser N em execução: acusar isso seria falso
    // positivo.
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "for i in 0..3 { run_shell(\"true\"); }\nprint(\"fim\");");
    cmd(home.path())
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--strict"])
        .assert()
        .success()
        .stdout(predicates::str::contains("fim"));
}

#[test]
fn json_devolve_dado_em_vez_de_prosa() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "run_shell(\"true\"); run_shell(\"true\"); print(\"oi\");");
    let out = cmd(home.path())
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let j: serde_json::Value = serde_json::from_slice(&out.stdout).expect("--json tem que ser JSON válido");
    assert_eq!(j["exit_code"], 0);
    assert_eq!(j["prim_total"], 2);
    assert_eq!(j["calls_avoided"], 1);
    assert_eq!(j["prims"]["run_shell"], 2);
    assert_eq!(j["output"], "oi\n");
}

#[test]
fn replace_all_in_glob_troca_em_lote_e_diz_o_que_tocou() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "versao ANTIGA aqui").unwrap();
    fs::write(dir.path().join("b.txt"), "outra ANTIGA linha\nANTIGA de novo").unwrap();
    fs::write(dir.path().join("c.txt"), "sem nada").unwrap();

    let s = escreve(
        dir.path(),
        r#"
let tocados = replace_all_in_glob("*.txt", "ANTIGA", "NOVA");
print("n=" + tocados.len());
"#,
    );
    cmd(home.path())
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("n=2"));

    assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "versao NOVA aqui");
    assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "outra NOVA linha\nNOVA de novo");
    assert_eq!(fs::read_to_string(dir.path().join("c.txt")).unwrap(), "sem nada");
}

#[test]
fn replace_all_in_glob_respeita_dry_run() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "ANTIGA").unwrap();
    let s = escreve(dir.path(), r#"print(replace_all_in_glob("*.txt", "ANTIGA", "NOVA").len());"#);
    cmd(home.path())
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--dry-run"])
        .assert()
        .success()
        .stderr(predicates::str::contains("[dry-run] replace_all_in_glob"));
    assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "ANTIGA");
}

#[test]
fn idioms_imprime_a_folha() {
    let home = tempfile::tempdir().unwrap();
    cmd(home.path())
        .arg("idioms")
        .assert()
        .success()
        .stdout(predicates::str::contains("MUTA E DEVOLVE"))
        .stdout(predicates::str::contains("git rev-parse --absolute-git-dir"))
        .stdout(predicates::str::contains("QUANDO NÃO USAR"));
}

#[test]
fn list_marca_script_sem_execucao_como_obsoleto() {
    // #81: `.codemode/` e versionado no repo, entao migracao one-shot fica pra
    // sempre. Das 9 bibliotecas reais auditadas em 20/08, 4 dos 9 scripts
    // distintos eram one-shots de issue ja fechada, cada um replicado em 19
    // worktrees do Orca. Quem roda `codemode list` via o morto ao lado do vivo.
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join(".codemode");
    fs::create_dir_all(&lib).unwrap();
    fs::write(lib.join("vivo.rhai"), "// desc: roda\nprint(\"ok\");\n").unwrap();
    fs::write(lib.join("morto.rhai"), "// desc: one-shot de issue fechada\nprint(\"x\");\n").unwrap();

    // Da historico a biblioteca rodando UM dos scripts: sem isso a guarda de
    // evidencia cala a marca, que e o comportamento do teste seguinte.
    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("run")
        .arg(lib.join("vivo.rhai"))
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success();

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("list")
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let linha_morto = texto.lines().find(|l| l.contains("morto.rhai")).unwrap();
    let linha_vivo = texto.lines().find(|l| l.contains("vivo.rhai")).unwrap();
    assert!(linha_morto.contains("obsoleto?"), "{linha_morto}");
    assert!(!linha_vivo.contains("obsoleto?"), "{linha_vivo}");
    assert!(texto.contains("git rm"), "diz o que fazer: {texto}");
}

#[test]
fn list_nao_marca_nada_quando_a_biblioteca_inteira_e_sem_historico() {
    // Se NENHUM script da pasta tem execucao registrada, "0x" nao e sinal de
    // morte -- e ausencia de dado. A telemetria e local e pode ser mais nova
    // que a biblioteca. Sem esta guarda, uma pasta com 6 scripts saia com 6
    // marcas, o que nao informa nada.
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join(".codemode");
    fs::create_dir_all(&lib).unwrap();
    fs::write(lib.join("a.rhai"), "// desc: um\nprint(\"a\");\n").unwrap();
    fs::write(lib.join("b.rhai"), "// desc: dois\nprint(\"b\");\n").unwrap();

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("list")
        .arg("--workdir")
        .arg(dir.path())
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(!texto.contains("obsoleto?"), "sem evidencia, sem marca: {texto}");
}
