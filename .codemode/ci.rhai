// desc: espelha o gate do CI (test + clippy) num tool-call, antes de abrir PR.
// uso:  codemode run ci.rhai --timeout 300
//
// Espelha `.github/workflows/ci.yml` LINHA POR LINHA, de propósito: um gate
// local que checa outra coisa é pior que gate nenhum, porque dá confiança
// falsa. A versão anterior deste arquivo dizia "fmt + clippy + test" e rodava
// `cargo test --release` seguido de `cargo test` -- a suíte duas vezes, clippy
// nenhuma. Dois PRs quebraram no CI por lint que este script deveria ter pego.
//
// `cargo fmt` NÃO entra, e não é esquecimento: o CI não checa formatação, a
// `main` não é rustfmt-formatada, e um `cargo fmt --all` reescreve 18 arquivos
// que não têm nada a ver com o seu diff.
//
// Precisa de `--timeout 300`: o padrão de 30s é do script inteiro, e uma suíte
// fria passa disso. O `--cmd-timeout` (600s por comando) já cobre o caso de um
// cargo travado, então o timeout global aqui é só folga, não proteção.

let falhou = [];

print("== cargo test --all-targets ==");
let t = run_shell_full("cargo test --all-targets");
print(t.stdout);
if !t.success {
    print(t.stderr);
    falhou.push("test");
}

print("== cargo clippy --all-targets -- -D warnings ==");
let c = run_shell_full("cargo clippy --all-targets -- -D warnings");
if !c.success {
    // O clippy escreve o achado no stderr; sem isto o gate diria "falhou" sem
    // dizer onde.
    print(c.stderr);
    falhou.push("clippy");
}

print("== git status ==");
print(run_shell("git status"));

if falhou.len() > 0 {
    throw "gate local falhou: " + join(falhou, ", ");
}
print("gate local OK -- test e clippy passaram, igual ao CI");
