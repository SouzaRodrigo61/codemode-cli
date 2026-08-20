//! Comando trivial resolvido em processo (#29).
//!
//! A regra que faz esta otimização ser segura: a saída tem que ser
//! **idêntica** à do binário real. Cada teste compara com o comando de
//! verdade, não com o que eu acho que ele imprime.

use assert_cmd::Command;
use std::fs;
use std::path::Path;

fn cmd() -> Command {
    let mut c = Command::cargo_bin("codemode").unwrap();
    c.env("CODEMODE_NO_TELEMETRY", "1");
    c
}

/// Roda o comando pelo codemode e pelo shell de verdade, no mesmo diretório.
fn compara(dir: &Path, comando: &str) -> (String, String) {
    let script = dir.join("s.rhai");
    fs::write(&script, format!("print(run_shell(\"{comando}\"));")).unwrap();
    let saida = cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.to_str().unwrap()])
        .output()
        .unwrap();
    let pelo_codemode = String::from_utf8_lossy(&saida.stdout).to_string();

    let real = std::process::Command::new("sh")
        .arg("-c")
        .arg(comando)
        .current_dir(dir)
        .output()
        .unwrap();
    let pelo_shell = String::from_utf8_lossy(&real.stdout).to_string();
    (pelo_codemode, pelo_shell)
}

fn cenario() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "linha um\nlinha dois\nlinha tres\n").unwrap();
    fs::write(dir.path().join("b.txt"), "outro arquivo\n").unwrap();
    fs::create_dir_all(dir.path().join("sub")).unwrap();
    fs::write(dir.path().join(".oculto"), "nao aparece\n").unwrap();
    dir
}

#[test]
fn cat_bate_com_o_cat_de_verdade() {
    let dir = cenario();
    let (nosso, real) = compara(dir.path(), "cat a.txt");
    assert_eq!(nosso.trim_end(), real.trim_end());
    assert!(nosso.contains("linha dois"));

    let (nosso, real) = compara(dir.path(), "cat a.txt b.txt");
    assert_eq!(nosso.trim_end(), real.trim_end());
}

#[test]
fn ls_bate_com_o_ls_de_verdade() {
    let dir = cenario();
    let (nosso, real) = compara(dir.path(), "ls");
    let mut a: Vec<&str> = nosso.split_whitespace().collect();
    let mut b: Vec<&str> = real.split_whitespace().collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "ls tem que listar o mesmo conjunto");
    assert!(!nosso.contains("oculto"), "ls sem -a não mostra oculto");
}

#[test]
fn echo_head_basename_dirname_batem() {
    let dir = cenario();
    // `print` acrescenta uma quebra à saída do run_shell, então a comparação
    // é sobre o conteúdo, não sobre a quebra final.
    for comando in ["echo oi mundo", "head -n 2 a.txt", "basename /a/b/c.txt", "dirname /a/b/c.txt"] {
        let (nosso, real) = compara(dir.path(), comando);
        assert_eq!(nosso.trim_end(), real.trim_end(), "divergiu em `{comando}`");
    }
}

#[test]
fn test_de_arquivo_devolve_o_mesmo_codigo() {
    let dir = cenario();
    // `run_shell` anexa "[exit code: N]" quando o comando falha: é assim que
    // o script enxerga fracasso.
    // Depois do #78, `run_shell` LANÇA em exit != 0, e `test` usa o exit code
    // como booleano -- entao quem quer o codigo usa `run_shell_full`, que e o
    // que o contrato novo manda. O pre-voo ja apontava `path_exists()` para
    // este caso antes mesmo da mudanca.
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
print("existe=[" + run_shell("test -f a.txt") + "]");
let nao = run_shell_full("test -f nao-existe.txt");
print("nao_exit=" + nao.exit_code + " sucesso=" + nao.success);
print("dir=[" + run_shell("test -d sub") + "]");
"#,
    )
    .unwrap();
    cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("existe=[]"))
        // O invariante que este teste protege segue o mesmo: o `test` em
        // processo devolve o MESMO codigo do binario real.
        .stdout(predicates::str::contains("nao_exit=1 sucesso=false"))
        .stdout(predicates::str::contains("dir=[]"));
}

#[test]
fn mutacoes_de_arquivo_acontecem_de_verdade() {
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
run_shell("mkdir -p novo/fundo");
run_shell("touch novo/vazio.txt");
run_shell("cp a.txt copia.txt");
run_shell("mv b.txt movido.txt");
run_shell("rm copia.txt");
print("fim");
"#,
    )
    .unwrap();
    cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success();

    assert!(dir.path().join("novo/fundo").is_dir());
    assert!(dir.path().join("novo/vazio.txt").is_file());
    assert!(!dir.path().join("copia.txt").exists());
    assert!(dir.path().join("movido.txt").is_file());
    assert!(!dir.path().join("b.txt").exists());
}

#[test]
fn caminho_de_fora_do_workdir_continua_indo_pro_shell() {
    // O confinamento de caminho vale para as PRIMITIVAS de arquivo; comando
    // de shell nunca foi confinado assim (é o modelo documentado: denylist
    // para o destrutivo, sandbox para read_file/write_file). O que este teste
    // trava é que a otimização não MUDA esse comportamento: caminho que o
    // sandbox recusa cai no shell e se comporta como sempre se comportou.
    let dir = cenario();
    let fora = tempfile::tempdir().unwrap();
    fs::write(fora.path().join("segredo.txt"), "conteudo de fora\n").unwrap();

    let comando = format!("cat {}/segredo.txt", fora.path().display());
    let (nosso, real) = compara(dir.path(), &comando);
    assert_eq!(nosso.trim_end(), real.trim_end(), "tem que ser idêntico ao shell");
}

#[test]
fn forma_desconhecida_nao_e_resolvida_em_processo() {
    // Flag fora da lista não pode ser "quase certa". `ls -la` sai do caminho
    // nativo -- e cai no roteamento do RTK, que é o comportamento de antes
    // (por isso a saída não é a do `ls` cru: vem filtrada).
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(&script, "print(run_shell(\"ls -la\"));").unwrap();
    let saida = cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    let texto = String::from_utf8_lossy(&saida.stdout);
    // O caminho nativo esconderia os ocultos; o comando real com -a mostra.
    assert!(texto.contains(".oculto"), "`-la` tem que sair do caminho nativo: {texto}");
}

#[test]
fn dry_run_nao_executa_nem_o_caminho_nativo() {
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"run_shell("rm a.txt");"#).unwrap();
    cmd()
        .args([
            "run",
            script.to_str().unwrap(),
            "--workdir",
            dir.path().to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .stderr(predicates::str::contains("[dry-run] run_shell"));
    assert!(dir.path().join("a.txt").exists(), "--dry-run não pode apagar nada");
}

#[test]
fn pre_voo_aponta_a_primitiva_equivalente() {
    // O caminho nativo (#29) só rende se o script parar de escrever shell
    // pro que já é primitiva. Código não força, mas avisa na hora certa (#31).
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        "let c = run_shell(\"cat a.txt\");\nlet achados = run_shell(\"grep -rn oi .\");\nprint(c.len() + achados.len());\n",
    )
    .unwrap();
    cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stderr(predicates::str::contains("read_file(caminho)"))
        .stderr(predicates::str::contains("linha 2"));
}

#[test]
fn pipe_nao_recebe_sugestao_errada() {
    // `cat x | wc -l` não tem equivalente direto: sugerir read_file ali seria
    // conselho errado.
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(&script, "print(run_shell(\"cat a.txt | wc -l\"));").unwrap();
    let saida = cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&saida.stderr);
    assert!(!err.contains("read_file(caminho)"), "não pode sugerir pra pipeline: {err}");
}
