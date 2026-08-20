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
                    "nenhum script recente em {} ({e}) -- rode um script antes, ou passe --from",
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
    println!("biblioteca em {}", dir.display());
    println!("{:-<78}", "");
    for p in &scripts {
        let nome = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let fonte = std::fs::read_to_string(p).unwrap_or_default();
        let n = execucoes(&nome, &entradas);
        let usa_args = fonte.contains("ARGS");
        println!(
            "  {:<26} {:>4}x{}  {}",
            nome,
            n,
            if usa_args { "  --arg" } else { "       " },
            descricao(&fonte).unwrap_or_else(|| "(sem // desc:)".into())
        );
    }
    println!("{:-<78}", "");
    println!("rode qualquer um por nome puro: codemode run <nome>.rhai");
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

PRIMITIVAS
  read_file  write_file  write_file_force  append_file  edit_file
  replace_all_in_glob  glob  grep  path_exists
  run_shell  run_shell_full  run_shell_confirmed  parallel_shell  http_get
  write_file SOBRESCREVE o arquivo inteiro -- pra acrescentar use append_file
  run_shell_full devolve #{{stdout, stderr, exit_code, success}} pra decidir no script

STDLIB QUE COSTUMAM PROCURAR
  join(lista, sep)  lines(texto)  trimmed(s)  to_json(x)  from_json(s)
  basename(p)  dirname(p)  path_exists(p)

LIMITES
  --timeout      script inteiro (0 desliga)
  --cmd-timeout  um comando de shell (0 = espera blocante, sem vigilância)
  --vm-idle      tempo sem primitiva nenhuma: é o que pega `loop {{}}`

GIT WORKTREE
  Dentro de um worktree, `.git` é ARQUIVO, não diretório: escrever em
  `.git/QUALQUER_COISA` falha com "not a directory". Use
  `git rev-parse --absolute-git-dir`.

BIBLIOTECA
  codemode list                  o que este repo já tem
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
