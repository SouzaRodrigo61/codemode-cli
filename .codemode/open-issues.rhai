// desc: abre uma issue no GitHub por bloco "### titulo" de um arquivo de spec.
// uso:  codemode run open-issues.rhai --arg <spec.md> [--arg <owner/repo>] [--arg dry]
// "dry" lista o que seria aberto sem chamar o gh.
// Existe porque abrir N issues era N chamadas gh separadas -- agora e uma so.
if ARGS.len() < 1 {
    print("uso: codemode run open-issues.rhai --arg <spec.md> [--arg <owner/repo>]");
} else {
    let spec = read_file(ARGS[0]);
    let repo_flag = if ARGS.len() > 1 { " -R " + ARGS[1] } else { "" };
    let dry = ARGS.len() > 2 && ARGS[2] == "dry";
    let tmp = ".codemode/.issue-body.md";
    let blocks = spec.split("\n### ");
    let criadas = 0;
    let falhas = 0;
    for b in blocks {
        let t = b;
        t.trim();
        if t.is_empty() { continue; }
        // O primeiro bloco carrega o cabecalho do arquivo antes do 1o "### ";
        // blocos sem titulo proprio sao ignorados de proposito.
        if t.starts_with("#") && !t.starts_with("### ") { continue; }
        let t2 = replaced(t, "### ", "");
        let i = t2.index_of("\n");
        let title = if i < 0 { t2 } else { t2.sub_string(0, i) };
        let body = if i < 0 { "" } else { t2.sub_string(i + 1) };
        title.trim();
        if dry {
            criadas += 1;
            print("dry  [" + title + "] corpo=" + body.len() + " chars");
            continue;
        }
        write_file(tmp, body);
        let r = run_shell_full("gh issue create" + repo_flag + " --title \"" + title + "\" --body-file " + tmp);
        if r.success {
            criadas += 1;
            print("ok   " + title + " -> " + r.stdout);
        } else {
            falhas += 1;
            print("FALHA " + title + " :: " + r.stderr);
        }
    }
    // O corpo e arquivo de trabalho, nao artefato do repo: sem isto ele
    // sobrava no working tree e um `git add -A` de outro script acabava
    // commitando (aconteceu no PR #56).
    run_shell("rm -f " + tmp);
    print("---");
    print("criadas: " + criadas + "  falhas: " + falhas);
}
