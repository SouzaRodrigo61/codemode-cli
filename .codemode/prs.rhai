// desc: estado dos PRs abertos -- CI, mergeable e título -- numa chamada só.
// uso:  codemode run prs.rhai                  todos os abertos
//       codemode run prs.rhai --arg 73 --arg 74  só esses
//
// Existe porque acompanhar N PRs era N chamadas `gh pr view` seguidas, e é o
// que mais se repete numa rodada com vários PRs em voo. A saída sai ordenada
// pelo número, não pela ordem em que o gh respondeu.
//
// Usa `from_json` em vez de `--jq`: a expressão do jq exige aspas simples, que
// o Rhai não aceita dentro de string, e o parse do lado de cá é o mesmo custo.

fn campo(mapa, chave, padrao) {
    if !mapa.contains(chave) { return padrao; }
    let v = mapa[chave];
    // Check em execução vem com `conclusion` VAZIA, não ausente -- tratar só o
    // ausente deixava a linha sair como "test: bench:" sem dizer nada.
    if type_of(v) == "()" { return padrao; }
    if type_of(v) == "string" && v == "" { return padrao; }
    v
}

let numeros = [];
if ARGS.len() > 0 {
    for a in ARGS { numeros.push(a); }
} else {
    let lista = run_shell_full("gh pr list --state open --json number --limit 30");
    if !lista.success {
        throw "gh pr list falhou: " + lista.stderr;
    }
    for p in from_json(lista.stdout) { numeros.push("" + p.number); }
}

if numeros.len() == 0 {
    print("nenhum PR aberto");
} else {
    for n in numeros {
        let r = run_shell_full("gh pr view " + n + " --json number,title,mergeable,statusCheckRollup");
        if !r.success {
            print("#" + n + "  ERRO: " + r.stderr);
        } else {
            let d = from_json(r.stdout);
            let checks = [];
            for c in campo(d, "statusCheckRollup", []) {
                // Check ainda rodando não traz `conclusion`; sem o padrão a
                // linha inteira viraria unit e sumiria da saída.
                checks.push(c.name + ":" + campo(c, "conclusion", "rodando"));
            }
            let estado = if checks.len() == 0 { "sem check" } else { join(checks, " ") };
            print("#" + d.number + "  " + estado + "  " + campo(d, "mergeable", "?"));
            print("      " + d.title);
        }
    }
}
