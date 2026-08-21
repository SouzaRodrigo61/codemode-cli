// desc: fecha o trabalho -- commit, push e PR -- numa chamada so.
// uso:  codemode run ship.rhai --arg <arquivo-mensagem> [--arg <arquivo-corpo-pr>]
// O arquivo de mensagem tem o assunto na 1a linha e o corpo no resto. Os
// arquivos sao copiados pra dentro do git dir real (que num worktree e um
// diretorio fora do repo, nao o ".git" local) e so entao removidos daqui --
// se ficassem, o "git add -A" os commitaria junto.
fn copiar_para(origem, destino) {
    let c = run_shell_full("cp " + origem + " " + destino);
    if !c.success {
        print("FALHA ao mover " + origem + ": " + c.stderr);
        return false;
    }
    run_shell("rm -f " + origem);
    return true;
}

if ARGS.len() < 1 {
    print("uso: codemode run ship.rhai --arg <arquivo-mensagem> [--arg <arquivo-corpo-pr>]");
} else {
    let g = run_shell_full("git rev-parse --absolute-git-dir");
    if !g.success {
        print("FALHA: nao e um repo git -- " + g.stderr);
    } else {
        let gitdir = g.stdout;
        gitdir.trim();
        let msg = read_file(ARGS[0]);
        let i = msg.index_of("\n");
        let assunto = if i < 0 { msg } else { msg.sub_string(0, i) };
        let msg_path = gitdir + "/CM_MSG";
        let corpo_pr = msg_path;
        if copiar_para(ARGS[0], msg_path) {
            if ARGS.len() > 1 {
                let pr_path = gitdir + "/CM_PR";
                if copiar_para(ARGS[1], pr_path) { corpo_pr = pr_path; }
            }
            let add = run_shell_full("git add -A");
            if !add.success { print("FALHA no add: " + add.stderr); }
            let c = run_shell_full("git commit -F " + msg_path);
            if !c.success {
                print("FALHA no commit: " + c.stderr + c.stdout);
            } else {
                print(c.stdout);
                let p = run_shell_full("git push -u origin HEAD");
                if !p.success {
                    print("FALHA no push: " + p.stderr);
                } else {
                    // Mesma armadilha que o #84 corrigiu no open-issues.rhai,
                    // e que me pegou de verdade: assunto com aspas duplas
                    // fecha a string do shell e o `gh` recebe argumento picado.
                    // `replaced`, nao `replace` -- o segundo muta e devolve unit.
                    let assunto_seguro = replaced(assunto, "\"", "\\\"");
                    let pr = run_shell_full("gh pr create --title \"" + assunto_seguro + "\" --body-file " + corpo_pr);
                    print(if pr.success { "PR: " + pr.stdout } else { "FALHA no PR: " + pr.stderr });
                }
            }
        }
    }
}
