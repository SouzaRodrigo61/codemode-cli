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

#[test]
fn grep_nativo_respeita_gitignore() {
    // O walker antigo descia em tudo: num repo com target/ de GBs a
    // primitiva levava segundos e perdia pro shell (#28).
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".gitignore"), "ignorado/\n").unwrap();
    fs::create_dir_all(dir.path().join("ignorado")).unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/a.rs"), "fn alvo_da_busca() {}\n").unwrap();
    fs::write(dir.path().join("ignorado/b.rs"), "fn alvo_da_busca() {}\n").unwrap();
    // Binário: não pode nem ser lido inteiro, nem casar.
    fs::write(dir.path().join("src/bin.dat"), [0u8, 1, 2, 3, b'a', b'l', b'v', b'o']).unwrap();
    std::process::Command::new("git").arg("init").arg("-q").current_dir(dir.path()).status().unwrap();

    let s = escreve(dir.path(), r#"print(grep("alvo_da_busca", "."));"#);
    let out = cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    let saida = String::from_utf8_lossy(&out.stdout);
    assert!(saida.contains("src/a.rs"), "devia achar no arquivo versionado: {saida}");
    assert!(!saida.contains("ignorado/"), "não pode entrar no que o .gitignore exclui: {saida}");
    assert!(!saida.contains("bin.dat"), "binário não entra na busca: {saida}");
}

#[test]
fn grep_nativo_devolve_resultado_ordenado() {
    // Walker paralelo devolve fora de ordem; resultado de busca precisa ser
    // reproduzível entre execuções.
    let dir = tempfile::tempdir().unwrap();
    for nome in ["c.txt", "a.txt", "b.txt"] {
        fs::write(dir.path().join(nome), "marcador\n").unwrap();
    }
    let s = escreve(dir.path(), r#"print(grep("marcador", "."));"#);
    let primeira = cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    let segunda = cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(primeira.stdout, segunda.stdout, "duas buscas iguais têm que dar a mesma saída");
    let saida = String::from_utf8_lossy(&primeira.stdout);
    let ordem: Vec<&str> =
        saida.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| l.split(':').next()).collect();
    let mut ordenado = ordem.clone();
    ordenado.sort();
    assert_eq!(ordem, ordenado, "saída deve vir ordenada por caminho: {saida}");
}

#[test]
fn cache_de_resolucao_nao_sobrevive_a_uma_mutacao() {
    // O cache do #37 só vale entre mutações: um symlink criado no meio da
    // execução não pode ser resolvido por um cache montado antes dele.
    let dir = tempfile::tempdir().unwrap();
    let fora = tempfile::tempdir().unwrap();
    fs::write(fora.path().join("segredo.txt"), "de fora\n").unwrap();
    fs::create_dir_all(dir.path().join("ponte")).unwrap();
    fs::write(dir.path().join("ponte/segredo.txt"), "de dentro\n").unwrap();

    let s = escreve(
        dir.path(),
        &format!(
            r#"
// Resolve uma vez com o diretório real...
print("antes=" + trimmed(read_file("ponte/segredo.txt")));
// ...troca o diretório por um symlink pra fora...
run_shell("rm ponte/segredo.txt");
run_shell("rmdir ponte");
run_shell("ln -s {fora} ponte");
// ...e a segunda leitura tem que ser barrada, não servida do cache.
try {{
    let x = read_file("ponte/segredo.txt");
    print("VAZOU=" + trimmed(x));
}} catch (e) {{
    print("bloqueado");
}}
"#,
            fora = fora.path().display()
        ),
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("antes=de dentro"))
        .stdout(predicates::str::contains("bloqueado"));
}

#[test]
fn glob_respeita_gitignore_mas_obedece_pedido_explicito() {
    // Mesma regra do grep (#28): o .gitignore governa onde a gente entra
    // sozinho, nunca o que foi pedido pelo nome.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::create_dir_all(dir.path().join("build/fundo")).unwrap();
    fs::write(dir.path().join("src/a.rs"), "// codigo\n").unwrap();
    fs::write(dir.path().join("src/b.rs"), "// codigo\n").unwrap();
    fs::write(dir.path().join("build/gerado.rs"), "// artefato\n").unwrap();
    fs::write(dir.path().join("build/fundo/outro.rs"), "// artefato\n").unwrap();
    std::process::Command::new("git").arg("init").arg("-q").current_dir(dir.path()).status().unwrap();

    let s = escreve(
        dir.path(),
        r#"
let tudo = glob("**/*.rs");
print("normal=" + join(tudo, ","));
let explicito = glob("build/**/*.rs");
print("explicito=" + explicito.len());
"#,
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        // ordenado e sem os artefatos ignorados
        .stdout(predicates::str::contains("normal=src/a.rs,src/b.rs"))
        // pedido pelo nome entra mesmo estando no .gitignore
        .stdout(predicates::str::contains("explicito=2"));
}

#[test]
fn glob_devolve_caminho_que_read_file_aceita_e_ordenado() {
    let dir = tempfile::tempdir().unwrap();
    for nome in ["c.txt", "a.txt", "b.txt"] {
        fs::write(dir.path().join(nome), "x\n").unwrap();
    }
    let s = escreve(
        dir.path(),
        r#"
let g = glob("*.txt");
print("ordem=" + join(g, ","));
let total = 0;
for f in g { total += read_file(f).len(); }
print("lidos=" + total);
"#,
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("ordem=a.txt,b.txt,c.txt"))
        .stdout(predicates::str::contains("lidos=6"));
}

#[test]
fn glob_com_padrao_absoluto_encontra_em_vez_de_devolver_vazio() {
    // #60: o padrao era casado contra o caminho RELATIVO ao root, entao
    // padrao absoluto percorria o diretorio certo e nunca casava nada --
    // devolvia [] sem erro, sem aviso. Resultado errado sem sinal e a mesma
    // familia do `replace` devolvendo unit: custa token duas vezes, porque o
    // script roda em falso e depois alguem refaz na mao.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("docs/a.md"), "a").unwrap();
    fs::write(dir.path().join("docs/b.md"), "b").unwrap();

    let raiz = fs::canonicalize(dir.path()).unwrap();
    let s = escreve(
        dir.path(),
        &format!(
            r#"
let abs = glob("{raiz}/docs/*.md");
print("abs=" + abs.len());
print("primeiro=" + abs[0]);
print("rel=" + glob("docs/*.md").len());
"#,
            raiz = raiz.display()
        ),
    );

    let esperado = format!("primeiro={}/docs/a.md", raiz.display());
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("abs=2"))
        // Padrao absoluto devolve caminho absoluto: e o que o chamador pediu,
        // e o que ele vai passar adiante pro read_file.
        .stdout(predicates::str::contains(esperado))
        // E o padrao relativo continua se comportando igual.
        .stdout(predicates::str::contains("rel=2"));
}

#[test]
fn glob_absoluto_alcanca_extra_root() {
    // O caso que motivou a issue: auditar dois escopos (o do projeto e o do
    // usuario) numa execucao so. Sem alcancar `--extra-root`, o script tinha
    // que ser reescrito para rodar com o HOME de workdir.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(b.path().join("x.md"), "x").unwrap();
    fs::write(b.path().join("y.md"), "y").unwrap();
    let outro = fs::canonicalize(b.path()).unwrap();

    let s = escreve(a.path(), &format!(r#"print("n=" + glob("{}/*.md").len());"#, outro.display()));

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
        .stdout(predicates::str::contains("n=2"));
}

#[test]
fn glob_absoluto_fora_de_qualquer_root_e_erro_explicito() {
    // Nao devolve vazio: recusa com a mesma mensagem que o resto do sandbox
    // ja da. Ausencia de resultado e ambigua; recusa nao e.
    let dir = tempfile::tempdir().unwrap();
    let fora = tempfile::tempdir().unwrap();
    fs::write(fora.path().join("segredo.md"), "nao").unwrap();
    let proibido = fs::canonicalize(fora.path()).unwrap();

    let s = escreve(dir.path(), &format!(r#"print(glob("{}/*.md").len());"#, proibido.display()));

    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("outside sandbox workdir"));
}

#[test]
fn glob_absoluto_sem_curinga_encontra_o_arquivo_exato() {
    // Canto que o #60 nao cobriu: quando o padrao absoluto nomeia um ARQUIVO
    // e nao tem curinga, o walker devolve so ele e o caminho relativo sai
    // vazio -- que era descartado, devolvendo [] em silencio. Mesma classe de
    // falha que a issue corrigia, sobrevivendo dentro da propria correcao.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("um.md"), "conteudo").unwrap();
    fs::write(dir.path().join("dois.md"), "conteudo").unwrap();
    let raiz = fs::canonicalize(dir.path()).unwrap();

    let s = escreve(
        dir.path(),
        &format!(
            r#"
let exato = glob("{raiz}/um.md");
print("exato=" + exato.len());
print("caminho=" + exato[0]);
print("curinga=" + glob("{raiz}/*.md").len());
print("relativo=" + glob("um.md").len());
"#,
            raiz = raiz.display()
        ),
    );

    let esperado = format!("caminho={}/um.md", raiz.display());
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("exato=1"))
        .stdout(predicates::str::contains(esperado))
        .stdout(predicates::str::contains("curinga=2"))
        // Relativo sem curinga ja funcionava e continua igual.
        .stdout(predicates::str::contains("relativo=1"));
}

#[test]
fn grep_alinha_o_caminho_com_a_forma_do_argumento() {
    // #71: o `rg` decidia sozinho -- relativo ao cwd para diretorio sob o
    // workdir, absoluto para diretorio em --extra-root. Mesma chamada, dois
    // formatos, e quem consome nao tem como saber qual esperar. Uma auditoria
    // comparando `glob` (absoluto desde o #60) com `grep` reportou "18 agentes
    // sem o bloco" quando os 18 tinham: nenhum erro, so resultado errado.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("docs/a.md"), "achar isto\n").unwrap();
    let raiz = fs::canonicalize(dir.path()).unwrap();

    let s = escreve(
        dir.path(),
        &format!(
            r#"
print("REL=" + grep("achar", "docs"));
print("ABS=" + grep("achar", "{raiz}/docs"));
"#,
            raiz = raiz.display()
        ),
    );

    let esperado_abs = format!("ABS={}/docs/a.md:1:", raiz.display());
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("REL=docs/a.md:1:"))
        .stdout(predicates::str::contains(esperado_abs));
}

#[test]
fn grep_em_extra_root_devolve_absoluto_para_argumento_absoluto() {
    // Fora do workdir, caminho relativo nem faria sentido: o formato tem que
    // vir do argumento, nao de onde o diretorio calhou de estar.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fs::write(b.path().join("x.md"), "achar isto\n").unwrap();
    let outro = fs::canonicalize(b.path()).unwrap();

    let s = escreve(a.path(), &format!(r#"print("R=" + grep("achar", "{}"));"#, outro.display()));
    let esperado = format!("R={}/x.md:1:", outro.display());

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
        .stdout(predicates::str::contains(esperado));
}

#[test]
fn grep_casa_com_glob_no_mesmo_script() {
    // O teste que reproduz a falha real: comparar o resultado do glob com o do
    // grep dentro do mesmo script tem que funcionar com padrao absoluto.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("um.md"), "marcador\n").unwrap();
    fs::write(dir.path().join("dois.md"), "sem nada\n").unwrap();
    let raiz = fs::canonicalize(dir.path()).unwrap();

    let s = escreve(
        dir.path(),
        &format!(
            r#"
let arqs = glob("{raiz}/*.md");
let achados = grep("marcador", "{raiz}");
let com = 0;
for a in arqs {{ if achados.contains(a) {{ com += 1; }} }}
print("com=" + com + " de " + arqs.len());
"#,
            raiz = raiz.display()
        ),
    );

    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("com=1 de 2"));
}

#[test]
fn glob_com_prefixo_literal_inexistente_erra_em_vez_de_devolver_vazio() {
    // #72: o chamador NOMEOU o diretorio. Se nao existe, o provavel e typo, e
    // devolver [] deixa o script seguir sem fazer nada -- resultado errado sem
    // sinal. `read_file` de arquivo inexistente erra; glob era o outlier.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("docs/a.md"), "a").unwrap();

    let s = escreve(dir.path(), r#"print(glob("dosc/*.md").len());"#);
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("does not exist"))
        .stderr(predicates::str::contains("path_exists"));
}

#[test]
fn glob_sem_prefixo_literal_continua_devolvendo_vazio_quando_nada_casa() {
    // "Nada casou" e resposta legitima quando o padrao nao nomeia diretorio
    // nenhum: `glob("**/*.zzz")` comeca no root, que sempre existe.
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("docs/a.md"), "a").unwrap();

    let s = escreve(
        dir.path(),
        r#"
print("curinga=" + glob("**/*.zzz").len());
print("prefixo_ok=" + glob("docs/*.zzz").len());
print("acha=" + glob("docs/*.md").len());
"#,
    );
    cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("curinga=0"))
        // Prefixo que EXISTE e nao casa nada segue devolvendo [].
        .stdout(predicates::str::contains("prefixo_ok=0"))
        .stdout(predicates::str::contains("acha=1"));
}

#[test]
fn timeout_global_nomeia_o_comando_em_voo() {
    // #79: a mensagem era só "script exceeded Ns timeout". Uma das 5 falhas de
    // uso real medidas no #59 foi exit 124 num script com quatro
    // `run_shell_full` (docker, ls, gh api): sem saber qual pendurou, a
    // execucao inteira virou lixo e o script teve que ser reescrito só para
    // bissectar. O watchdog mata o processo, entao a unica forma de dizer o
    // que travou e registrar antes de bloquear.
    let dir = tempfile::tempdir().unwrap();
    let s = escreve(dir.path(), "print(\"antes\");\nrun_shell(\"sh -c \\\"sleep 5\\\"\");\n");

    let out = cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--timeout", "2"])
        .output()
        .unwrap();
    let erro = String::from_utf8_lossy(&out.stderr);
    assert!(erro.contains("watchdog"), "{erro}");
    assert!(erro.contains("run_shell"), "nomeia a primitiva: {erro}");
    assert!(erro.contains("sleep 5"), "nomeia o comando: {erro}");
}

#[test]
fn comando_muito_longo_e_truncado_na_mensagem_de_timeout() {
    // A mensagem serve para identificar, nao para reproduzir: comando de 300
    // caracteres despejado no stderr seria outro problema de contexto.
    let dir = tempfile::tempdir().unwrap();
    let enchimento = "a".repeat(200);
    let s = escreve(
        dir.path(),
        &format!("run_shell(\"sh -c \\\"sleep 5 # {enchimento}\\\"\");\n"),
    );

    let out = cmd()
        .args(["run", s.to_str().unwrap(), "--workdir", dir.path().to_str().unwrap(), "--timeout", "2"])
        .output()
        .unwrap();
    let erro = String::from_utf8_lossy(&out.stderr);
    // A asserção olha a LINHA do watchdog, não o stderr inteiro: o aviso de
    // primitiva única também fala do mesmo comando, e misturar os dois fez o
    // teste acusar a linha errada quando o #86 mudou o aviso.
    let linha = erro
        .lines()
        .find(|l| l.contains("watchdog"))
        .unwrap_or_else(|| panic!("sem linha de watchdog em: {erro}"));
    assert!(linha.contains("..."), "truncou: {linha}");
    assert!(!linha.contains(&"a".repeat(100)), "nao despeja o comando inteiro: {linha}");
}
