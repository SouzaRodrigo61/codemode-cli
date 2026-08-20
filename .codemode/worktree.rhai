// desc: cria (ou reaproveita) um worktree isolado a partir do origin/main mais recente.
// uso:  codemode run worktree.rhai --arg <branch> [--arg <caminho>]
// Existe porque o ritual da skill trabalho-isolado-real -- fetch, worktree add,
// branch, confirmar -- era 4 chamadas de shell separadas toda vez.
if ARGS.len() < 1 {
    print("uso: codemode run worktree.rhai --arg <branch> [--arg <caminho>]");
} else {
    let branch = ARGS[0];
    let path = if ARGS.len() > 1 { ARGS[1] } else { "../" + branch };
    let f = run_shell_full("git fetch origin main");
    if !f.success {
        print("FALHA no fetch: " + f.stderr);
    } else {
        let w = run_shell_full("git worktree add -b " + branch + " " + path + " origin/main");
        if !w.success {
            print("worktree add falhou (pode ja existir): " + w.stderr);
        }
        print(run_shell("git worktree list"));
        print("--- HEAD do worktree ---");
        print(run_shell_full("git -C " + path + " log --oneline -1").stdout);
        print("caminho: " + path);
    }
}
