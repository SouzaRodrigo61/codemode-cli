//! Pré-voo, stdlib, `check` e `--dry-run` (#13, #14, #15, #16, #17).

use assert_cmd::Command;
use std::fs;
use std::path::Path;

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
fn simbolo_inexistente_barra_antes_de_qualquer_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    // A escrita vem ANTES da chamada inexistente: antes do pré-voo, ela
    // acontecia e só então o script morria.
    let s = escreve(dir.path(), "write_file(\"saida.txt\", \"x\");\nlet a = [1,2];\nlet t = join(a, \",\");\nnao_existe_mesmo(t);\n");

    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("nao_existe_mesmo"));

    assert!(!dir.path().join("saida.txt").exists(), "nenhuma escrita pode ter acontecido");
}

#[test]
fn sugere_o_nome_registrado_mais_proximo() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "read_fil(\"x\");");
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("read_file"));
}

#[test]
fn builtins_comuns_do_rhai_nao_dao_falso_positivo() {
    // O risco real do pré-voo é recusar script válido. Este exercita o
    // vocabulário que um script de verdade usa.
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(
        dir.path(),
        r#"
let a = [3, 1, 2];
a.push(4);
a.sort();
let m = #{ chave: "valor" };
let ks = m.keys();
let txt = "  oi mundo ";
let partes = txt.split(" ");
print("len=" + a.len() + " ks=" + ks.len() + " partes=" + partes.len());
print("tipo=" + type_of(a) + " sub=" + txt.sub_string(2, 2));
print("contem=" + txt.contains("oi") + " idx=" + txt.index_of("mundo"));
for x in a { print(x); }
fn dobro(x) { x * 2 }
print(dobro(21));
"#,
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("42"));
}

#[test]
fn erro_de_sintaxe_mostra_a_linha_e_o_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "let a = 1;\nlet b = ;\nprint(b);\n");
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("   2 | let b = ;"))
        .stderr(predicates::str::contains("^"));
}

#[test]
fn erro_de_runtime_tambem_mostra_o_trecho() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "print(\"antes\");\nread_file(\"nao-existe.txt\");\n");
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("   2 | read_file"));
}

#[test]
fn check_valida_sem_executar() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "write_file(\"nao-devia-existir.txt\", \"x\");\nrun_shell(\"true\");\n");

    cmd()
        .args(["check", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("2 primitiva"));

    assert!(!dir.path().join("nao-devia-existir.txt").exists(), "check não executa");
}

#[test]
fn check_falha_em_script_quebrado() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "nao_existe();");
    cmd()
        .args(["check", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("não existe"));
}

#[test]
fn check_resolve_script_da_biblioteca_pelo_nome() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".codemode")).unwrap();
    fs::write(dir.path().join(".codemode/lib.rhai"), "run_shell(\"true\");").unwrap();
    cmd()
        .args(["check", "lib.rhai", "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn dry_run_le_mas_nao_escreve_nem_executa() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("origem.txt"), "conteudo original").unwrap();
    let s = escreve(
        dir.path(),
        r#"
let c = read_file("origem.txt");
print("li=" + c);
write_file("novo.txt", "nao devia existir");
edit_file("origem.txt", "original", "trocado");
run_shell("touch tocado.txt");
"#,
    );

    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--dry-run"])
        .assert()
        .success()
        .stdout(predicates::str::contains("li=conteudo original"))
        .stderr(predicates::str::contains("[dry-run] write_file"))
        .stderr(predicates::str::contains("[dry-run] edit_file"))
        .stderr(predicates::str::contains("[dry-run] run_shell"));

    assert!(!dir.path().join("novo.txt").exists());
    assert!(!dir.path().join("tocado.txt").exists());
    assert_eq!(fs::read_to_string(dir.path().join("origem.txt")).unwrap(), "conteudo original");
}

#[test]
fn stdlib_nova_responde() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("existe.txt"), "a\nb\nc\n").unwrap();
    let s = escreve(
        dir.path(),
        r#"
print("join=" + join(["a", "b"], "-"));
print("lines=" + lines(read_file("existe.txt")).len());
print("trimmed=[" + trimmed("  x  ") + "]");
print("json=" + to_json(#{ n: 1, s: "dois" }));
let v = from_json("{\"lista\":[1,2,3]}");
print("from=" + v.lista.len());
print("base=" + basename("/a/b/c.txt") + " dir=" + dirname("/a/b/c.txt"));
print("existe=" + path_exists("existe.txt") + " nao=" + path_exists("nao.txt"));
"#,
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("join=a-b"))
        .stdout(predicates::str::contains("lines=3"))
        .stdout(predicates::str::contains("trimmed=[x]"))
        .stdout(predicates::str::contains("\"n\":1"))
        .stdout(predicates::str::contains("from=3"))
        .stdout(predicates::str::contains("base=c.txt dir=/a/b"))
        .stdout(predicates::str::contains("existe=true nao=false"));
}

#[test]
fn from_json_invalido_e_erro_claro() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "from_json(\"{ nao e json\");");
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("JSON inválido"));
}

#[test]
fn idioma_de_outra_linguagem_vira_dica() {
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "let f = (x) => x + 1;\n");
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("arrow function"));
}
