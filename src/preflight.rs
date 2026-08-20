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

/// Vocabulário que sabemos existir sem perguntar ao engine: as nossas
/// primitivas, a stdlib que registramos, os operadores, e os builtins de
/// Rhai que aparecem em script de verdade.
///
/// Existe porque `gen_fn_signatures` formata a assinatura COMPLETA de todas
/// as 223 funções registradas só pra extrair o nome antes do parêntese --
/// 0,83ms em toda execução, a maior fatia isolada de um script trivial
/// (#40). Errar pra menos aqui é seguro: nome que não estiver nesta lista
/// simplesmente cai no caminho lento, que continua correto.
/// Formas que o PARSER resolve, não o registro de funções: não aparecem em
/// `gen_fn_signatures` e por isso ficam fora do teste de invariante.
const PALAVRAS_DA_LINGUAGEM: &[&str] = &[
    "+", "-", "*", "/", "%", "**", "==", "!=", "<", "<=", ">", ">=", "&&", "||", "!", "&", "|",
    "^", "..", "..=", "in", "call", "curry", "eval", "type_of", "is_def_var", "is_def_fn",
];

/// Builtins de Rhai que os scripts usam de verdade. Cada nome aqui É
/// registrado pelo engine -- o teste `lista_rapida_e_subconjunto_do_que_o_engine_registra`
/// garante isso, e foi ele que pegou `max` sumindo quando o #41 trocou os
/// pacotes.
const CONHECIDOS_RAPIDOS: &[&str] = &[
    // núcleo
    "print", "debug", "len", "is_empty", "to_string", "to_int", "to_float",
    "parse_int", "parse_float", "abs", "sign", "min", "max", "floor", "round", "sqrt",
    "exp", "ln", "log", "sin", "cos", "tan", "int",
    // string
    "trim", "to_upper", "to_lower", "sub_string", "split", "index_of", "contains", "replace",
    "starts_with", "ends_with", "chars", "pad", "crop", "truncate", "bytes",
    "split_rev", "make_upper", "make_lower", "to_chars",
    // array e mapa
    "push", "pop", "insert", "remove", "clear", "shift", "extract", "keys", "values",
    "sort", "reverse", "filter", "map", "reduce", "reduce_rev", "some", "all", "find", "find_map",
    "dedup", "drain", "retain", "append", "get", "set", "fill_with",
    "mixin", "range", "chop", "take", "for_each",
    // tempo
    "timestamp", "elapsed",
];

/// Nomes registrados no engine (inclui os pacotes padrão do Rhai) mais as
/// funções que o próprio script define. Caminho LENTO: só roda quando sobrou
/// nome que o vocabulário rápido não conhece.
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
/// A stdlib que nós registramos (#14) -- separada das primitivas porque
/// estas não contam para a guarda de trivialidade.
pub const STDLIB: &[&str] =
    &["join", "lines", "trimmed", "replaced", "to_json", "from_json", "basename", "dirname"];

pub const PRIMITIVES: &[&str] = &[
    "read_file", "write_file", "write_file_force", "append_file", "edit_file", "run_shell",
    "run_shell_full", "run_shell_confirmed", "grep", "glob", "http_get", "replace_all_in_glob",
    "parallel_shell", "path_exists", "read_files",
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

/// Troca o conteúdo de cada literal de string por espaços, preservando o
/// comprimento. Sem isto, `print("str=" + txt.trim())` era acusado de
/// atribuir o `trim`: o `=` estava DENTRO da string. Falso positivo achado
/// pelo próprio teste de vocabulário (#41).
fn sem_literais(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dentro = false;
    let mut escapando = false;
    for c in s.chars() {
        if dentro {
            if escapando {
                escapando = false;
                out.push(' ');
                continue;
            }
            match c {
                '\\' => {
                    escapando = true;
                    out.push(' ');
                }
                '"' => {
                    dentro = false;
                    out.push('"');
                }
                _ => out.push(' '),
            }
        } else if c == '"' {
            dentro = true;
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out
}

/// Dado um trecho que começa no `(`, devolve o que vem depois do `)` que o
/// fecha. None se os parênteses não fecham.
fn fecha_parenteses(s: &str) -> Option<&str> {
    let mut nivel = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => nivel += 1,
            ')' => {
                nivel -= 1;
                if nivel == 0 {
                    return Some(&s[i + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn mutating_assignments(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, linha) in source.lines().enumerate() {
        if linha.trim_start().starts_with("//") {
            continue;
        }
        // Por instrução, não por linha: `let o = r.stdout; o.trim();` é o
        // idioma CERTO (muta e usa a própria variável), e olhar a linha
        // inteira o acusava por causa do `=` da instrução anterior.
        let limpa = sem_literais(linha);
        for instrucao in limpa.split(';') {
            for (m, dica) in MUTADORES_UNIT {
            let agulha = format!(".{m}(");
            if let Some(at) = instrucao.find(&agulha) {
                let antes = &instrucao[..at];
                let atribui = antes.contains('=') && !antes.contains("==") && !antes.contains("!=");
                // `s.trim().len()` é o mesmo erro em outra forma: encadear no
                // retorno de quem devolve unit.
                let encadeia = fecha_parenteses(&instrucao[at + agulha.len() - 1..])
                    .map(|resto| resto.trim_start().starts_with('.'))
                    .unwrap_or(false);
                if atribui || encadeia {
                    out.push(format!(
                        "linha {}: `{}` MUTA em lugar e devolve () -- {} isso dá unit, não valor. {}.",
                        i + 1,
                        m,
                        if atribui { "atribuir" } else { "encadear em" },
                        dica
                    ));
                }
            }
            }
        }
    }
    out
}

/// Comando de shell que tem primitiva equivalente (#31). O caminho nativo
/// (#29) já resolve vários deles em processo, mas a primitiva ainda ganha:
/// devolve dado tipado em vez de texto pra reparsear, e não depende de o
/// formato do comando ser o mesmo em macOS e Linux.
const EQUIVALENTES: &[(&str, &str)] = &[
    ("cat", "read_file(caminho)"),
    ("ls", "glob(\"*\")"),
    ("find", "glob(\"**/*.ext\")"),
    ("grep", "grep(padrao, caminho) -- respeita .gitignore e é ~200x mais rápido"),
    ("wc", "lines(read_file(caminho)).len()"),
    ("sed", "replace_all_in_glob(padrao, velho, novo)"),
    ("test", "path_exists(caminho)"),
    ("head", "lines(read_file(caminho))"),
    ("jq", "from_json(texto)"),
];

/// Acha `run_shell("comando ...")` no fonte e sugere a primitiva. Olha o
/// literal, não o AST, porque o comando montado em runtime não dá pra
/// julgar -- e avisar errado custaria mais do que resolve.
pub fn shell_com_equivalente(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, linha) in source.lines().enumerate() {
        if linha.trim_start().starts_with("//") {
            continue;
        }
        for gatilho in ["run_shell(\"", "run_shell_full(\""] {
            let Some(at) = linha.find(gatilho) else { continue };
            let resto = &linha[at + gatilho.len()..];
            let Some(fim) = resto.find('"') else { continue };
            let comando = &resto[..fim];
            // Pipe, redirecionamento e encadeamento não têm equivalente
            // direto: a sugestão seria errada.
            if comando.contains('|') || comando.contains('>') || comando.contains("&&") {
                continue;
            }
            let Some(primeira) = comando.split_whitespace().next() else { continue };
            if let Some((_, alternativa)) =
                EQUIVALENTES.iter().find(|(nome, _)| *nome == primeira)
            {
                out.push(format!(
                    "linha {}: `{}` em run_shell -- use {} (primitiva nativa custa ~0,02ms; \
                     spawn de processo custa 1,5-8,5ms)",
                    i + 1,
                    primeira,
                    alternativa
                ));
            }
        }
    }
    out.truncate(5);
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
    let chamados = called_names(&ast);
    // Caminho rápido (#40): quase todo script só chama o que já conhecemos
    // estaticamente. `gen_fn_signatures` -- que formata a assinatura inteira
    // de 200+ funções -- só roda se sobrou algum nome fora do vocabulário
    // conhecido, e custava 0,83ms em TODA execução.
    let suspeitos: Vec<&String> = chamados
        .iter()
        .filter(|n| {
            let s = n.as_str();
            !CONHECIDOS_RAPIDOS.contains(&s)
                && !PALAVRAS_DA_LINGUAGEM.contains(&s)
                && !PRIMITIVES.contains(&s)
                && !STDLIB.contains(&s)
                && !ast.iter_functions().any(|f| f.name == s)
        })
        .collect();
    let conhecidos =
        if suspeitos.is_empty() { BTreeSet::new() } else { known_names(engine, &ast) };
    for nome in suspeitos {
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
    let mut avisos: Vec<String> =
        foreign_idiom_hints(source).into_iter().map(str::to_string).collect();
    avisos.extend(shell_com_equivalente(source));
    Ok(Report {
        prim_calls,
        has_loop: has_loop(&ast),
        warnings: avisos,
        ast,
    })
}

#[cfg(test)]
mod testes {
    use super::*;

    /// A lista rápida (#40) só pode ENCURTAR o caminho, nunca inventar
    /// vocabulário: um nome listado aqui que o engine não registra vira erro
    /// de runtime que o pré-voo deveria ter pego. Foi assim que a troca de
    /// pacotes do #41 quase deixou `max` passar.
    #[test]
    fn lista_rapida_e_subconjunto_do_que_o_engine_registra() {
        let mut engine = crate::primitives::nova_engine();
        let sandbox = crate::sandbox::Sandbox::new(std::path::Path::new(".")).unwrap();
        crate::primitives::register(&mut engine, sandbox, Vec::new(), crate::primitives::new_counter());
        let registrados: std::collections::BTreeSet<String> = engine
            .gen_fn_signatures(true)
            .into_iter()
            .map(|s| s.split('(').next().unwrap_or_default().trim().to_string())
            .collect();

        let ausentes: Vec<&str> = CONHECIDOS_RAPIDOS
            .iter()
            .chain(STDLIB.iter())
            .chain(PRIMITIVES.iter())
            .copied()
            .filter(|n| !registrados.contains(*n))
            .collect();
        assert!(ausentes.is_empty(), "nomes na lista rápida que o engine NÃO registra: {ausentes:?}");
    }

    /// O sentido inverso do teste acima, que faltava -- e por onde o
    /// `read_files` (#63) passou: ficou registrado no engine e FORA da lista,
    /// entao `codemode check` reportava "0 primitiva(s) referenciada(s)" para
    /// um script que so usava ele, e a contagem que decide desperdicio/
    /// `--strict` ignorava a chamada.
    ///
    /// Comparar contra um engine SEM o nosso `register` isola exatamente o
    /// vocabulario que nos adicionamos, sem arrastar a stdlib do Rhai.
    #[test]
    fn tudo_que_o_nosso_register_adiciona_esta_em_alguma_lista() {
        fn nomes(engine: &rhai::Engine) -> std::collections::BTreeSet<String> {
            engine
                .gen_fn_signatures(true)
                .into_iter()
                .map(|s| s.split('(').next().unwrap_or_default().trim().to_string())
                .collect()
        }

        let base = nomes(&crate::primitives::nova_engine());
        let mut engine = crate::primitives::nova_engine();
        let sandbox = crate::sandbox::Sandbox::new(std::path::Path::new(".")).unwrap();
        crate::primitives::register(&mut engine, sandbox, Vec::new(), crate::primitives::new_counter());
        let depois = nomes(&engine);

        let conhecidos: std::collections::BTreeSet<&str> =
            STDLIB.iter().chain(PRIMITIVES.iter()).copied().collect();
        let fora: Vec<&String> = depois
            .difference(&base)
            .filter(|n| !conhecidos.contains(n.as_str()))
            .collect();
        assert!(
            fora.is_empty(),
            "o engine registra nomes que o pré-voo não conhece -- some com a contagem \
             de primitivas e com o `check`: {fora:?}"
        );
    }
}
