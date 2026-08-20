//! Pré-voo: tudo que dá pra saber sobre um script ANTES de executar a
//! primeira primitiva (issues #13, #15, #17, #21).
//!
//! O motivo é concreto: até aqui, um `Function not found: join` na linha 8
//! só aparecia depois de cinco `run_shell` já terem rodado, e os avisos de
//! idioma só disparavam no caminho de falha -- ou seja, com as escritas já
//! aplicadas em disco. Compilar e resolver os símbolos custa ~5ms e não
//! toca em nada.

use rhai::{ASTNode, Engine, Expr, Stmt, AST};
use std::collections::BTreeSet;

pub struct Report {
    pub ast: AST,
    /// Chamadas a primitiva do codemode encontradas estaticamente. Não
    /// conta repetição dentro de laço -- por isso `has_loop` existe.
    pub prim_calls: usize,
    pub has_loop: bool,
    pub warnings: Vec<String>,
}

/// Nomes que o script chamou, do AST -- não de regex sobre o fonte.
fn called_names(ast: &AST) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    ast.walk(&mut |path| {
        match path.last() {
            Some(ASTNode::Expr(Expr::FnCall(x, _))) => {
                out.insert(x.name.to_string());
            }
            Some(ASTNode::Stmt(Stmt::FnCall(x, _))) => {
                out.insert(x.name.to_string());
            }
            _ => {}
        }
        true
    });
    out
}

fn has_loop(ast: &AST) -> bool {
    let mut found = false;
    ast.walk(&mut |path| {
        if matches!(
            path.last(),
            Some(ASTNode::Stmt(Stmt::For(..))) | Some(ASTNode::Stmt(Stmt::While(..))) | Some(ASTNode::Stmt(Stmt::Do(..)))
        ) {
            found = true;
        }
        true
    });
    found
}

/// Nomes registrados no engine (inclui os pacotes padrão do Rhai) mais as
/// funções que o próprio script define.
fn known_names(engine: &Engine, ast: &AST) -> BTreeSet<String> {
    let mut set: BTreeSet<String> = engine
        .gen_fn_signatures(true)
        .into_iter()
        .map(|sig| sig.split('(').next().unwrap_or_default().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for f in ast.iter_functions() {
        set.insert(f.name.to_string());
    }
    // Operadores e formas que o Rhai resolve internamente e que nem sempre
    // aparecem na lista de assinaturas.
    for extra in [
        "+", "-", "*", "/", "%", "**", "==", "!=", "<", "<=", ">", ">=", "&&", "||", "!", "&", "|", "^",
        "..", "..=", "in", "call", "curry", "eval", "type_of", "print", "debug", "is_def_var", "is_def_fn",
    ] {
        set.insert(extra.to_string());
    }
    set
}

/// As primitivas do codemode -- o que distingue "script que colapsa
/// tool-calls" de "script que só faz conta".
pub const PRIMITIVES: &[&str] = &[
    "read_file", "write_file", "write_file_force", "append_file", "edit_file", "run_shell",
    "run_shell_full", "run_shell_confirmed", "grep", "glob", "http_get", "replace_all_in_glob",
    "parallel_shell", "path_exists",
];

/// Sugere o registrado mais parecido (distância de edição curta ou prefixo
/// em comum), pra mensagem não ser só "não existe".
fn parecido(nome: &str, conhecidos: &BTreeSet<String>) -> Option<String> {
    let mut melhor: Option<(usize, String)> = None;
    for c in conhecidos {
        let d = distancia(nome, c);
        if d <= 2 && melhor.as_ref().is_none_or(|(bd, _)| d < *bd) {
            melhor = Some((d, c.clone()));
        }
    }
    melhor.map(|(_, c)| c)
}

fn distancia(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut linha: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut anterior = linha[0];
        linha[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let atual = linha[j + 1];
            linha[j + 1] = (linha[j] + 1).min(atual + 1).min(anterior + usize::from(ca != cb));
            anterior = atual;
        }
    }
    linha[b.len()]
}

/// Três linhas de contexto e um cursor na coluna (issue #15): sem isso o
/// chamador precisa recontar as linhas de um heredoc que ele mesmo escreveu.
pub fn excerpt(source: &str, pos: rhai::Position) -> Option<String> {
    let linha = pos.line()?;
    let col = pos.position().unwrap_or(1);
    let linhas: Vec<&str> = source.lines().collect();
    if linha == 0 || linha > linhas.len() {
        return None;
    }
    let mut out = String::new();
    let inicio = linha.saturating_sub(2).max(1);
    for n in inicio..=linha {
        out.push_str(&format!("{n:>4} | {}\n", linhas[n - 1]));
    }
    out.push_str(&format!("     | {}^\n", " ".repeat(col.saturating_sub(1))));
    Some(out)
}

/// Método que MUTA em lugar e devolve unit, usado como valor. Vira erro de
/// pré-voo, não aviso: foi exatamente assim que um batch apagou 70 arquivos
/// em 2026-08-19 -- e aquela execução não deu erro nenhum (issue #17).
const MUTADORES_UNIT: &[(&str, &str)] = &[
    ("replace", "use replaced(s, velho, novo)"),
    ("trim", "use trimmed(s)"),
    ("push", "mute a variável e use ela mesma"),
    ("reverse", "mute a variável e use ela mesma"),
    ("truncate", "mute a variável e use ela mesma"),
    ("pad", "mute a variável e use ela mesma"),
    ("crop", "mute a variável e use ela mesma"),
];

pub fn mutating_assignments(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, linha) in source.lines().enumerate() {
        if linha.trim_start().starts_with("//") {
            continue;
        }
        // Por instrução, não por linha: `let o = r.stdout; o.trim();` é o
        // idioma CERTO (muta e usa a própria variável), e olhar a linha
        // inteira o acusava por causa do `=` da instrução anterior.
        for instrucao in linha.split(';') {
            for (m, dica) in MUTADORES_UNIT {
            let agulha = format!(".{m}(");
            if let Some(at) = instrucao.find(&agulha) {
                let antes = &instrucao[..at];
                let atribui = antes.contains('=') && !antes.contains("==") && !antes.contains("!=");
                if atribui {
                    out.push(format!(
                        "linha {}: `{}` MUTA em lugar e devolve () -- atribuir isso dá unit, não valor. {}.",
                        i + 1,
                        m,
                        dica
                    ));
                }
            }
            }
        }
    }
    out
}

/// Idiomas de outra linguagem. Continuam avisos: um `=>` dentro de string
/// não é erro, e falso positivo aqui custaria mais do que resolve.
pub fn foreign_idiom_hints(source: &str) -> Vec<&'static str> {
    const HINTS: &[(&str, &str)] = &[
        ("console.", "Rhai não tem console — use print(x)"),
        ("println!", "Rhai não é Rust — use print(x), sem macros"),
        ("format!", "Rhai não tem format! — use interpolação `texto ${x}` ou concatenação +"),
        ("function ", "funções em Rhai usam fn nome() { }, não function"),
        ("=>", "closure em Rhai é |x| expr, não arrow function =>"),
        ("===", "Rhai usa == e !=, não === / !=="),
        ("require(", "Rhai não tem require/import — só as funções nativas do codemode"),
        ("import ", "Rhai não tem import — só as funções nativas do codemode"),
        (".forEach", "Rhai não tem forEach — use for x in lista { }"),
        ("let mut ", "Rhai não usa mut — toda variável let já é mutável"),
    ];
    HINTS.iter().filter(|(pat, _)| source.contains(pat)).map(|(_, hint)| *hint).take(3).collect()
}

/// Compila, resolve símbolo e roda os linters. Erro aqui aborta antes da
/// primeira primitiva -- nenhum side-effect.
pub fn check(engine: &Engine, source: &str) -> Result<Report, Vec<String>> {
    let ast = match engine.compile(source) {
        Ok(ast) => ast,
        Err(e) => {
            let mut erros = vec![format!("erro de sintaxe: {e}")];
            if let Some(t) = excerpt(source, e.position()) {
                erros.push(t);
            }
            for h in foreign_idiom_hints(source) {
                erros.push(format!("dica: {h}"));
            }
            return Err(erros);
        }
    };

    let mut erros: Vec<String> = Vec::new();
    let conhecidos = known_names(engine, &ast);
    let chamados = called_names(&ast);
    for nome in &chamados {
        if !conhecidos.contains(nome) {
            let sugestao = parecido(nome, &conhecidos)
                .map(|s| format!(" -- você quis dizer `{s}`?"))
                .unwrap_or_else(|| {
                    " -- veja `codemode idioms` pra lista do que existe".to_string()
                });
            erros.push(format!("função `{nome}` não existe{sugestao}"));
        }
    }
    erros.extend(mutating_assignments(source));
    if !erros.is_empty() {
        for h in foreign_idiom_hints(source) {
            erros.push(format!("dica: {h}"));
        }
        return Err(erros);
    }

    let prim_calls = chamados.iter().filter(|n| PRIMITIVES.contains(&n.as_str())).count();
    Ok(Report {
        prim_calls,
        has_loop: has_loop(&ast),
        warnings: foreign_idiom_hints(source).into_iter().map(str::to_string).collect(),
        ast,
    })
}
