// desc: gate local do próprio codemode-cli (fmt + clippy + test) num tool-call.
// Uso: codemode run ci.rhai
print("== cargo test ==");
let r = run_shell_full("cargo test --release");
if !r.success {
    print(r.stderr);
    throw "cargo test falhou (exit " + r.exit_code + ")";
}
print(run_shell("cargo test"));
print("== git status ==");
print(run_shell("git status"));
