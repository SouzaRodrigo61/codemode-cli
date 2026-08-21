//! `codemode save` e `codemode list` (#20): o caminho que faz um script
//! inline virar ativo do repo em vez de lixo de scratchpad.
//!
//! A resolução por nome puro já existia (`read_script` procura em
//! `<workdir>/.codemode/`). O que faltava era ergonomia: na auditoria de 200
//! execuções, só 7% vinham de biblioteca -- 1 script versionado num repo, 6
//! em outro, e apenas um efetivamente reusado. Sem `list`, o agente nem sabe
//! que a biblioteca existe, e reescreve.

use crate::telemetry;
use std::path::Path;

pub struct SaveArgs {
    pub nome: String,
    pub workdir: std::path::PathBuf,
    pub from: Option<String>,
    pub desc: Option<String>,
    pub force: bool,
}

fn com_extensao(nome: &str) -> String {
    if nome.ends_with(".rhai") {
        nome.to_string()
    } else {
        format!("{nome}.rhai")
    }
}

pub fn save(args: SaveArgs) -> Result<i32, String> {
    let fonte = match args.from.as_deref() {
        Some("-") => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).map_err(|e| format!("stdin: {e}"))?;
            buf
        }
        Some(caminho) => std::fs::read_to_string(caminho).map_err(|e| format!("{caminho}: {e}"))?,
        None => {
            let ultimo = telemetry::home()
                .map(|h| h.join("last.rhai"))
                .ok_or("HOME indefinido: passe --from")?;
            std::fs::read_to_string(&ultimo).map_err(|e| {
                format!(
                    "nenhum script recente em {} ({e}) -- o fonte só é guardado para script \
                     vindo de stdin; para script que já está em disco, passe --from <caminho>",
                    ultimo.display()
                )
            })?
        }
    };

    let dir = args.workdir.join(".codemode");
    std::fs::create_dir_all(&dir).map_err(|e| format!("não consegui criar {}: {e}", dir.display()))?;
    let destino = dir.join(com_extensao(&args.nome));
    if destino.exists() && !args.force {
        return Err(format!("{} já existe -- use --force pra sobrescrever", destino.display()));
    }

    let mut conteudo = String::new();
    if let Some(d) = &args.desc {
        if !fonte.starts_with("// desc:") {
            conteudo.push_str(&format!("// desc: {d}\n"));
        }
    }
    conteudo.push_str(&fonte);
    std::fs::write(&destino, conteudo).map_err(|e| format!("não consegui escrever {}: {e}", destino.display()))?;
    println!("salvo: {}", destino.display());
    println!("rode com: codemode run {}", com_extensao(&args.nome));
    Ok(0)
}

/// Primeira linha `// desc:` do script, se houver.
fn descricao(fonte: &str) -> Option<String> {
    fonte
        .lines()
        .take(5)
        .find_map(|l| l.trim_start().strip_prefix("// desc:").map(|d| d.trim().to_string()))
}

fn execucoes(nome: &str, entradas: &[telemetry::Entry]) -> usize {
    entradas
        .iter()
        .filter(|e| match &e.name {
            Some(n) => n == nome || n.ends_with(&format!("/.codemode/{nome}")) || n.ends_with(&format!("/{nome}")),
            None => false,
        })
        .count()
}

/// Epoch da última execução do script, se houve alguma.
fn ultima_execucao(nome: &str, entradas: &[telemetry::Entry]) -> Option<u64> {
    entradas
        .iter()
        .filter(|e| match &e.name {
            Some(n) => n == nome || n.ends_with(&format!("/.codemode/{nome}")) || n.ends_with(&format!("/{nome}")),
            None => false,
        })
        .map(|e| e.ts)
        .max()
}

/// Dias sem rodar até o script ser marcado como candidato a remoção.
const DIAS_ATE_OBSOLETO: u64 = 30;

/// Script que nunca rodou, ou que não roda há mais de 30 dias, ganha marca.
///
/// Existe porque `.codemode/` é versionado no repo e migração one-shot fica
/// para sempre: das 9 bibliotecas reais auditadas em 20/08, **4 dos 9 scripts
/// distintos eram one-shots de issue já fechada** (`issue-723-migrate`,
/// `issue-725-inspect/migrate/verify`), e cada worktree do Orca carrega uma
/// cópia -- 19 delas. Quem roda `codemode list` vê o morto ao lado do vivo, com
/// o mesmo peso (#81).
///
/// Marca, não apaga: o dado é fraco de propósito -- a telemetria é local, então
/// script que só roda no CI de outra máquina aparece como obsoleto aqui. A
/// decisão é de quem lê.
///
/// `biblioteca_tem_historico` evita gritar lobo sem evidência: se NENHUM script
/// daquela pasta tem execução registrada, "0x" não é sinal de morte, é ausência
/// de dado -- a telemetria pode ser mais nova que a biblioteca, ou aquele repo
/// pode nunca ter rodado nesta máquina. Sem essa guarda, uma pasta com 6
/// scripts saía com 6 marcas, o que não informa nada.
fn marca_obsoleto(ultima: Option<u64>, agora: u64, biblioteca_tem_historico: bool) -> &'static str {
    if !biblioteca_tem_historico {
        return "";
    }
    match ultima {
        None => "  obsoleto?",
        Some(ts) if agora.saturating_sub(ts) > DIAS_ATE_OBSOLETO * 86_400 => "  obsoleto?",
        Some(_) => "",
    }
}

pub fn list(workdir: &Path) -> Result<i32, String> {
    let dir = workdir.join(".codemode");
    let mut scripts: Vec<std::path::PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "rhai"))
            .collect(),
        Err(_) => {
            println!("nenhuma biblioteca em {} -- crie com `codemode save <nome>`", dir.display());
            return Ok(0);
        }
    };
    scripts.sort();
    if scripts.is_empty() {
        println!("nenhum script em {} -- crie com `codemode save <nome>`", dir.display());
        return Ok(0);
    }

    let entradas = telemetry::load();
    let agora = telemetry::now_secs();
    println!("biblioteca em {}", dir.display());
    println!("{:-<78}", "");
    // Alguém desta pasta já rodou? Se não, a pasta não tem cobertura de
    // telemetria e nenhuma marca faz sentido.
    let tem_historico = scripts.iter().any(|p| {
        let nome = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        execucoes(&nome, &entradas) > 0
    });
    let mut obsoletos = 0;
    for p in &scripts {
        let nome = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let fonte = std::fs::read_to_string(p).unwrap_or_default();
        let n = execucoes(&nome, &entradas);
        let usa_args = fonte.contains("ARGS");
        let marca = marca_obsoleto(ultima_execucao(&nome, &entradas), agora, tem_historico);
        if !marca.is_empty() {
            obsoletos += 1;
        }
        println!(
            "  {:<26} {:>4}x{}{}  {}",
            nome,
            n,
            if usa_args { "  --arg" } else { "       " },
            marca,
            descricao(&fonte).unwrap_or_else(|| "(sem // desc:)".into())
        );
    }
    println!("{:-<78}", "");
    println!("rode qualquer um por nome puro: codemode run <nome>.rhai");
    if obsoletos > 0 {
        println!(
            "{obsoletos} marcado(s) `obsoleto?`: sem execução há {DIAS_ATE_OBSOLETO}+ dias nesta \
             máquina.\n  Migração one-shot de issue fechada é o caso comum -- apague com \
             `git rm .codemode/<nome>.rhai`.\n  A telemetria é local: script que só roda no CI de \
             outra máquina aparece aqui sem ter morrido."
        );
    }
    Ok(0)
}

/// A folha de armadilhas (#22). Mora no binário de propósito: CLAUDE.md,
/// AGENTS.md e skills não são herdados por subagente, mas o binário está no
/// PATH de todo mundo.
pub fn idioms() -> i32 {
    print!(
        r#"codemode: idiomas e armadilhas do Rhai (o que mais custa execução perdida)

SINTAXE
  fn nome(x) {{ ... }}        não `function`
  |x| x + 1                  não `=>`
  ==  !=                     não `===` / `!==`
  let x = 1;                 não existe `mut`: todo let já é mutável
  "texto ${{x}}"               interpolação só com aspas duplas
  ASPAS SIMPLES NÃO EXISTEM  'a' é caractere, não string

MÉTODO QUE MUTA E DEVOLVE ()  -- erro de pré-voo, não aviso
  s.replace(a, b)   -> use replaced(s, a, b)
  s.trim()          -> use trimmed(s)
  push/reverse/truncate/pad/crop: mutam a variável; use ela depois, não o retorno
  O idioma certo: `let o = r.stdout; o.trim(); print(o);`

run_shell LANÇA quando o comando falha
  Exit != 0 aborta o script, em TODOS os caminhos (sh, nativo, rtk).
  Falha esperada? use run_shell_full, que devolve
  #{{stdout, stderr, exit_code, success}} e não aborta.
  Comando que usa exit code como BOOLEANO (`test -f`, `grep -q`, `diff -q`)
  aborta o script: use a primitiva nativa (path_exists) ou run_shell_full.

PRIMITIVAS
  read_file  read_files  write_file  write_file_force  append_file  edit_file
  replace_all_in_glob  glob  grep  path_exists
  run_shell  run_shell_full  run_shell_confirmed  parallel_shell  http_get
  write_file SOBRESCREVE o arquivo inteiro -- pra acrescentar use append_file
  read_file(p, #{{lines: "120-180"}})  só o trecho; 1-based, inclusivo dos dois lados
  read_files([...])                    mapa caminho -> conteúdo, aceita a mesma faixa
                                       (paraleliza acima de ~400 arquivos; abaixo é serial)

GLOB
  glob("docs/*.md")     relativo ao workdir
  glob("/abs/*.md")     absoluto vale, e devolve caminho absoluto
  glob("dosc/*.md")     diretório literal que não existe é ERRO, não lista vazia
  glob("**/*.zzz")      nada casou -> [] (isso continua sendo resposta legítima)
  grep(padrão, caminho) o formato do caminho na saída segue o do argumento

STDLIB QUE COSTUMAM PROCURAR
  join(lista, sep)  lines(texto)  trimmed(s)  to_json(x)  from_json(s)
  basename(p)  dirname(p)  path_exists(p)

LIMITES
  --timeout       script inteiro, 30s (0 desliga). Suíte de teste dentro do
                  script FUNCIONA -- é este que precisa subir: --timeout 300
  --cmd-timeout   um comando de shell, 600s (0 = espera blocante)
  --vm-idle       tempo sem primitiva nenhuma: é o que pega `loop {{}}`
  --max-output    corte de segurança, 1 MiB
  --max-context   AVISA (não corta) acima de 64 KiB: saída também custa token
  --strict        recusa script que colapsa menos de 2 primitivas

GIT WORKTREE
  Dentro de um worktree, `.git` é ARQUIVO, não diretório: escrever em
  `.git/QUALQUER_COISA` falha com "not a directory". Use
  `git rev-parse --absolute-git-dir`.

BIBLIOTECA -- os dois diretórios chamados .codemode
  ~/.codemode/          estado, criado SOZINHO na 1a execução
                        runs.jsonl (telemetria) + last.rhai (último script)
  <repo>/.codemode/     biblioteca, versionada no git, NUNCA criada sozinha:
                        só `codemode save` a cria. `codemode run` não cria.

  codemode list                  o que este repo já tem -- RODE ANTES de fazer à mão
  codemode save <nome>           promove o último script pra .codemode/
  codemode run <nome>.rhai       roda por nome puro
  codemode check <script>        pré-voo sem executar
  codemode run x.rhai --dry-run  anuncia toda escrita/comando, não faz nenhum

QUANDO NÃO USAR
  Uma operação só (1 read, 1 edit, 1 comando) é mais barata em Bash direto.
  O ganho aparece a partir de 2, e é real a partir de 3.
"#
    );
    0
}
