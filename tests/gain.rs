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

    // Workdir de teste é tempdir, e tempdir é rascunho -- então o relatório
    // padrão (trabalho real) NÃO conta estas execuções, e `--bench` conta.
    // É a segmentação do #59 funcionando, não efeito colateral.
    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["gain", "--bench"])
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(texto.contains("Execuções:"), "{texto}");
    assert!(texto.contains("Tool-calls evitadas:"), "{texto}");
    assert!(texto.contains("desperdício"), "{texto}");

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["gain", "--json", "--bench"])
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
        .args(["gain", "--history", "--bench"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Últimas"));
}

#[test]
fn linha_corrompida_no_log_nao_quebra_o_relatorio() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join("runs.jsonl"), "{lixo\n{\"ts\":1,\"script\":\"a\",\"source\":\"file\",\"prims\":{\"run_shell\":2},\"prim_total\":2,\"out_bytes\":0,\"exit_code\":0,\"ms\":1,\"workdir\":\"/tmp\",\"kind\":\"real\"}\n").unwrap();

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["gain", "--json"])
        .assert()
        .success();
    let j: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(j["runs"], 1, "a linha corrompida é descartada, o resto conta");
}

// ---------------------------------------------------------------------------
// #59 -- segmentação: o relatório é sobre trabalho real, e bench/rascunho/self
// entram numa conta separada. Sem isso, na máquina onde a issue foi medida,
// 1.303 de 1.312 execuções eram o próprio codemode e o ganho saía inflado 145x.
// ---------------------------------------------------------------------------

/// Diretório que simula um repo de trabalho de verdade: fora de qualquer raiz
/// temporária (`CARGO_TARGET_TMPDIR` vive sob `target/`) e com `Cargo.toml`
/// próprio de OUTRO crate -- sem ele o probe sobe a árvore, encontra o
/// `Cargo.toml` do próprio codemode e classifica como "self", corretamente.
fn dir_de_trabalho(nome: &str) -> std::path::PathBuf {
    let base = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(nome);
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    fs::write(base.join("Cargo.toml"), "[package]\nname = \"app-de-teste\"\n").unwrap();
    base
}

#[test]
fn execucao_em_repo_de_trabalho_e_marcada_como_real() {
    let home = tempfile::tempdir().unwrap();
    let dir = dir_de_trabalho("gain_real");
    run_in(home.path(), &dir, r#"run_shell("true"); run_shell("true");"#).success();

    let l = linhas(home.path());
    assert_eq!(l[0]["kind"], "real", "workdir normal é trabalho real: {:?}", l[0]);

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("gain")
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(texto.contains("trabalho real"), "{texto}");
    assert!(texto.contains("Execuções:                   1"), "{texto}");
}

#[test]
fn execucao_em_diretorio_temporario_e_rascunho() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    run_in(home.path(), dir.path(), r#"run_shell("true");"#).success();

    let l = linhas(home.path());
    assert_eq!(l[0]["kind"], "bench", "tempdir é rascunho, não trabalho: {:?}", l[0]);

    // E o relatório padrão diz quantas ficaram de fora, em vez de omitir.
    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("gain")
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(texto.contains("nenhuma execução de trabalho real"), "{texto}");
    assert!(texto.contains("`codemode gain --bench`"), "{texto}");
}

#[test]
fn script_sob_bench_nao_conta_como_trabalho_real() {
    let home = tempfile::tempdir().unwrap();
    let dir = dir_de_trabalho("gain_bench_dir");
    fs::create_dir_all(dir.join("bench").join("casos")).unwrap();
    fs::write(dir.join("bench/casos/x.rhai"), r#"run_shell("true");"#).unwrap();

    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("run")
        .arg(dir.join("bench/casos/x.rhai"))
        .arg("--workdir")
        .arg(&dir)
        .assert()
        .success();

    let l = linhas(home.path());
    assert_eq!(l[0]["kind"], "bench", "caso sob bench/ é benchmark: {:?}", l[0]);
}

#[test]
fn linha_antiga_com_workdir_apagado_vira_desconhecida_e_nao_real() {
    // Sem `kind` (linha anterior ao #59) e com o workdir já removido: não dá
    // para provar o que era. Contar como real era o que inflava o número --
    // das 36 "reais" da máquina do #59, 27 eram worktrees de dev apagadas.
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("runs.jsonl"),
        "{\"ts\":1,\"script\":\"a\",\"source\":\"file\",\"prims\":{\"run_shell\":2},\
\"prim_total\":2,\"out_bytes\":0,\"exit_code\":0,\"ms\":1,\
\"workdir\":\"/nao/existe/mais/worktree-apagada\"}\n",
    )
    .unwrap();

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("gain")
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(texto.contains("Não classificáveis"), "a incerteza aparece: {texto}");
    assert!(texto.contains("nenhuma execução de trabalho real"), "{texto}");
}

#[test]
fn relatorio_expoe_bytes_por_execucao_e_maiores_despejos() {
    // Byte de contexto é o que custa token; tool-call evitada não paga nada
    // por si. Um script que evita 10 chamadas e despeja 200KB é prejuízo.
    let home = tempfile::tempdir().unwrap();
    let dir = dir_de_trabalho("gain_despejo");
    fs::write(dir.join("g.txt"), "x".repeat(4000)).unwrap();
    run_in(home.path(), &dir, r#"print(read_file("g.txt")); run_shell("true");"#).success();

    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("gain")
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(texto.contains("Saída por execução:"), "{texto}");
    assert!(texto.contains("Maiores despejos de contexto"), "{texto}");
    assert!(texto.contains("s.rhai"), "o despejador é nomeado: {texto}");
}

#[test]
fn telemetria_grava_o_verbo_do_shell_e_nao_o_argumento() {
    // #82: o runs.jsonl sabia que houve 6 `run_shell` e nada sobre o que eles
    // rodaram, entao nao havia como responder "qual comando merece virar
    // primitiva nativa?". O `read_files` do #63 foi escolhido por intuicao e a
    // medicao depois mostrou que so paga acima de 400 arquivos.
    let home = tempfile::tempdir().unwrap();
    let dir = dir_de_trabalho("verbos");
    run_in(
        home.path(),
        &dir,
        r#"run_shell("echo um"); run_shell("git --version"); run_shell("echo dois");"#,
    )
    .success();

    let l = linhas(home.path());
    let verbos = &l[0]["prims_shell"];
    assert_eq!(verbos["echo"], 2, "{verbos:?}");
    assert_eq!(verbos["git"], 1, "{verbos:?}");
}

#[test]
fn verbo_do_shell_e_so_a_primeira_palavra_sem_caminho_nem_argumento() {
    // A regra de privacidade da telemetria continua valendo: metadado, nunca
    // conteudo. `sh -c "curl -H Authorization: ..."` grava `sh`, e o caminho
    // do binario vira basename -- o diretorio diria onde as coisas estao
    // instaladas, que e mais do que a pergunta precisa.
    let home = tempfile::tempdir().unwrap();
    let dir = dir_de_trabalho("verbos_privacidade");
    run_in(
        home.path(),
        &dir,
        r#"run_shell("/bin/echo com-caminho"); run_shell_full("sh -c \"echo segredo-nao-vaza\"");"#,
    )
    .success();

    let l = linhas(home.path());
    let bruto = serde_json::to_string(&l[0]["prims_shell"]).unwrap();
    assert!(bruto.contains("\"echo\""), "basename, nao caminho: {bruto}");
    assert!(!bruto.contains("/bin/"), "caminho nao entra: {bruto}");
    assert!(bruto.contains("\"sh\""), "{bruto}");
    assert!(!bruto.contains("segredo"), "argumento NUNCA entra: {bruto}");
}

#[test]
fn recusa_do_strict_nao_conta_como_falha() {
    // #95: `--strict` recusa script que colapsa menos de duas primitivas -- a
    // defesa direta contra o desperdicio que a telemetria mede. Mas a recusa
    // gravava exit=2, que o `gain` contava como FALHA: ligar a guarda por
    // padrao pioraria justamente o numero que se quer baixar.
    let home = tempfile::tempdir().unwrap();
    let dir = dir_de_trabalho("recusa");
    fs::write(dir.join("a.txt"), "x").unwrap();

    // Uma primitiva so: a guarda recusa.
    let s = dir.join("uma.rhai");
    fs::write(&s, r#"print(read_file("a.txt"));"#).unwrap();
    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["run", s.to_str().unwrap(), "--workdir"])
        .arg(&dir)
        .arg("--strict")
        .assert()
        // O exit code CONTINUA 2: quem chama o binario precisa saber que nao rodou.
        .code(2);

    let l = linhas(home.path());
    assert_eq!(l[0]["kind"], "recusado", "{:?}", l[0]);
    assert_eq!(l[0]["exit_code"], 2, "{:?}", l[0]);

    // E nao aparece como falha no relatorio de trabalho real.
    let out = Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .arg("gain")
        .assert()
        .success();
    let texto = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(texto.contains("Recusadas pela guarda"), "aparece com nome proprio: {texto}");
    assert!(
        texto.contains("nenhuma execução de trabalho real"),
        "e nao entra na conta de trabalho real: {texto}"
    );
}

#[test]
fn erro_de_pre_voo_continua_contando_como_falha() {
    // A distincao que importa: recusa de GUARDA e a ferramenta decidindo que
    // nao vale a pena rodar. Erro de sintaxe e de funcao inexistente e falha
    // de quem escreveu o script, e tem que continuar pesando.
    let home = tempfile::tempdir().unwrap();
    let dir = dir_de_trabalho("erro_pre_voo");
    let s = dir.join("ruim.rhai");
    fs::write(&s, "read_filez(\"a.txt\"); glob(\"*\"); grep(\"x\", \".\");").unwrap();

    Command::cargo_bin("codemode")
        .unwrap()
        .env("CODEMODE_HOME", home.path())
        .args(["run", s.to_str().unwrap(), "--workdir"])
        .arg(&dir)
        .assert()
        .code(1);

    let l = linhas(home.path());
    assert_eq!(l[0]["kind"], "real", "erro do autor e trabalho real: {:?}", l[0]);
    assert_eq!(l[0]["exit_code"], 1);
}
