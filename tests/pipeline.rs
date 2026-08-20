//! Pipeline e redirecionamento sem `sh` (#30).
//!
//! Mesmo contrato do caminho nativo: a saída tem que ser idêntica à do
//! shell de verdade, e o que sai do subconjunto seguro volta pro `sh`.

use assert_cmd::Command;
use std::fs;
use std::path::Path;

fn cmd() -> Command {
    let mut c = Command::cargo_bin("codemode").unwrap();
    c.env("CODEMODE_NO_TELEMETRY", "1");
    c
}

fn compara(dir: &Path, comando: &str) -> (String, String) {
    let script = dir.join("s.rhai");
    fs::write(&script, format!("print(run_shell(\"{comando}\"));")).unwrap();
    let nosso = cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.to_str().unwrap()])
        .output()
        .unwrap();
    let real = std::process::Command::new("sh")
        .arg("-c")
        .arg(comando)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&nosso.stdout).trim_end().to_string(),
        String::from_utf8_lossy(&real.stdout).trim_end().to_string(),
    )
}

fn cenario() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "banana\nabacaxi\nbanana\ncaju\ndamasco\n").unwrap();
    dir
}

#[test]
fn pipelines_batem_com_o_shell() {
    let dir = cenario();
    for comando in [
        "cat a.txt | wc -l",
        "cat a.txt | head -n 2",
        "cat a.txt | sort",
        "cat a.txt | sort -u",
        "cat a.txt | sort | uniq",
        "cat a.txt | grep banana",
        "cat a.txt | grep -v banana",
        "cat a.txt | grep -c banana",
        "cat a.txt | tail -n 2",
        "echo um dois tres | wc -c",
        "cat a.txt | sort | head -n 2 | wc -l",
    ] {
        let (nosso, real) = compara(dir.path(), comando);
        assert_eq!(nosso, real, "divergiu em `{comando}`");
    }
}

#[test]
fn redirecionamento_escreve_o_mesmo_arquivo() {
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
run_shell("cat a.txt | sort > ordenado.txt");
run_shell("echo final | cat >> ordenado.txt");
print("ok");
"#,
    )
    .unwrap();
    cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success();

    let nosso = fs::read_to_string(dir.path().join("ordenado.txt")).unwrap();
    std::process::Command::new("sh")
        .arg("-c")
        .arg("cat a.txt | sort > esperado.txt; echo final | cat >> esperado.txt")
        .current_dir(dir.path())
        .status()
        .unwrap();
    let esperado = fs::read_to_string(dir.path().join("esperado.txt")).unwrap();
    assert_eq!(nosso, esperado);
}

#[test]
fn fora_do_subconjunto_seguro_volta_pro_shell() {
    let dir = cenario();
    // Substituição de comando, variável, glob, `;` e `&&`: só o shell resolve.
    for comando in [
        "echo $HOME | wc -c",
        "cat a.txt | grep banana ; echo fim",
        "echo um && echo dois",
        "cat *.txt | wc -l",
    ] {
        let (nosso, real) = compara(dir.path(), comando);
        assert_eq!(nosso, real, "divergiu em `{comando}` (devia ter ido pro sh)");
    }
}

#[test]
fn denylist_vale_em_qualquer_etapa_do_pipe() {
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(
        &script,
        r#"
try {
    run_shell("cat a.txt | rm -rf /");
    print("NAO DEVIA PASSAR");
} catch (e) {
    print("capturado indevidamente");
}
"#,
    )
    .unwrap();
    cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("denylist"));
}

#[test]
fn dry_run_nao_executa_pipeline() {
    let dir = cenario();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"run_shell("cat a.txt | sort > saida.txt");"#).unwrap();
    cmd()
        .args([
            "run",
            script.to_str().unwrap(),
            "--workdir",
            dir.path().to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success();
    assert!(!dir.path().join("saida.txt").exists(), "--dry-run não escreve");
}

#[test]
fn pipeline_com_entrada_grande_nao_trava() {
    // Se a escrita no stdin não fosse em thread, o buffer do pipe encheria e
    // travaria com entrada maior que ~64 KB.
    let dir = tempfile::tempdir().unwrap();
    let linhas: String = (0..20_000).map(|i| format!("linha {i}\n")).collect();
    fs::write(dir.path().join("grande.txt"), &linhas).unwrap();
    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print(run_shell("cat grande.txt | wc -l"));"#).unwrap();
    cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .timeout(std::time::Duration::from_secs(20))
        .assert()
        .success()
        .stdout(predicates::str::contains("20000"));
}

#[test]
fn comando_de_saida_minima_nao_paga_o_roteamento() {
    // `git rev-parse HEAD` são 40 caracteres: não há o que o filtro do RTK
    // comprima, só o que cobrar (#32). O teste garante que a saída é a do
    // git puro, sem passar por filtro.
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git").arg("init").arg("-q").current_dir(dir.path()).status().unwrap();
    fs::write(dir.path().join("x.txt"), "conteudo\n").unwrap();
    for args in [vec!["add", "-A"], vec!["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "inicial"]] {
        std::process::Command::new("git").args(&args).current_dir(dir.path()).status().unwrap();
    }

    let script = dir.path().join("s.rhai");
    fs::write(&script, r#"print(trimmed(run_shell("git rev-parse HEAD")));"#).unwrap();
    let saida = cmd()
        .args(["run", script.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    let texto = String::from_utf8_lossy(&saida.stdout);
    let sha = texto.trim();
    assert_eq!(sha.len(), 40, "devia ser o SHA puro, sem enfeite de filtro: {texto:?}");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
}
