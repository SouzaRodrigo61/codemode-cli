//! Hook de PreToolUse: o host manda o JSON da chamada na stdin e a resposta
//! reescreve o comando de shell para passar por `codemode exec`.
//!
//! É o mesmo contrato que o `rtk hook claude` já falava -- e **substitui**
//! ele, não soma: desde 0.2.0 o rtk é dependência de biblioteca deste
//! binário (README, "RTK lives inside codemode now, not next to it"), então
//! manter os dois no PreToolUse pagaria dois spawns e embrulharia o comando
//! em cima do embrulho. Um hook só entrega o filtro do rtk (in-process
//! quando o filtro está migrado), a denylist, o corte de saída e a
//! telemetria que alimenta `codemode gain`.

use crate::denylist;

/// Aspas simples de shell. O comando volta pro host como UM argumento
/// porque `exec_one` só trata argv de tamanho 1 como linha de shell -- é
/// isso que preserva pipe, `&&` e redirect. Sem aspas, `git log | head`
/// viraria dois argumentos e o pipe se perderia no caminho.
fn aspas(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Primeiro token de um prefixo, sem aspas e sem diretório: é com ele que
/// a idempotência do `--wrap` compara. O prefixo real chega com caminho
/// absoluto e aspas (`'/Users/x/.local/bin/caveman' shrink --`), mas o
/// comando que o agente escreve à mão vem curto (`caveman shrink -- ls`) --
/// comparar a linha inteira erraria os dois lados.
fn cabeca(prefixo: &str) -> Option<String> {
    let t = prefixo.split_whitespace().next()?.trim_matches(['\'', '"']);
    Some(t.rsplit('/').next()?.to_string())
}

/// `None` significa "passa cru": o host segue com o comando original e com
/// a decisão de permissão que ele já tomaria sozinho.
///
/// A denylist é o único portão aqui, e ela devolve `None` de propósito:
/// reescrever `rm -rf` seria pedir `allow` justamente para o que existe
/// para ser perguntado. O que a denylist pega continua chegando cru na
/// engine de permissão do host.
/// `wrap` embrulha a reescrita por fora: `<wrap> codemode exec -- '<cmd>'`.
/// Existe porque um host só aceita UMA reescrita por chamada -- dois hooks
/// que reescrevem a mesma chamada não se compõem, o último a responder
/// apaga o outro. Quem quiser encadear outro roteador (um compressor de
/// saída, por exemplo) passa ele aqui e a ordem deixa de ser sorteio.
/// codemode não sabe o que é o programa embrulhado, e é de propósito.
pub fn claude(entrada: &str, wrap: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(entrada).ok()?;
    if v.get("tool_name")?.as_str()? != "Bash" {
        return None;
    }
    let cmd = v.get("tool_input")?.get("command")?.as_str()?.trim();
    if cmd.is_empty() {
        return None;
    }
    // Idempotência: o host pode reentrar (retry, hook em cadeia, comando
    // que o próprio agente já escreveu roteado) e um segundo embrulho
    // rodaria codemode dentro de codemode -- dois processos para nada.
    if cmd.starts_with("codemode ") || cmd.starts_with("rtk ") {
        return None;
    }
    if denylist::check(cmd).is_some() {
        return None;
    }
    let wrap = wrap.map(str::trim).filter(|w| !w.is_empty());
    // Mesma idempotência do bloco acima, agora para o embrulho: uma linha
    // que já começa pelo programa do --wrap embrulharia ele em si mesmo.
    if let Some(c) = wrap.and_then(cabeca) {
        if cabeca(cmd).as_deref() == Some(c.as_str()) {
            return None;
        }
    }
    let reescrito = match wrap {
        Some(w) => format!("{w} codemode exec -- {}", aspas(cmd)),
        None => format!("codemode exec -- {}", aspas(cmd)),
    };
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "codemode auto-rewrite (rtk in-process + telemetria)",
                "updatedInput": { "command": reescrito }
            }
        })
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_(entrada: &str) -> Option<String> {
        claude(entrada, None)
    }

    fn payload(tool: &str, cmd: &str) -> String {
        serde_json::json!({
            "tool_name": tool,
            "tool_input": { "command": cmd },
            "cwd": "/tmp"
        })
        .to_string()
    }

    fn comando(saida: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(saida).unwrap();
        v["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn reescreve_comando_comum() {
        let s = claude_(&payload("Bash", "git status")).unwrap();
        assert_eq!(comando(&s), "codemode exec -- 'git status'");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    }

    #[test]
    fn linha_de_shell_vira_um_argumento_so() {
        // O pipe tem que sobreviver: um argumento só é linha de shell para
        // `exec_one`, dois ou mais seriam argv e o `| head` se perderia.
        let s = claude_(&payload("Bash", "git log --oneline | head -5")).unwrap();
        assert_eq!(comando(&s), "codemode exec -- 'git log --oneline | head -5'");
    }

    #[test]
    fn aspas_simples_no_comando_sobrevivem() {
        let s = claude_(&payload("Bash", "python3 -c 'print(1)'")).unwrap();
        assert_eq!(comando(&s), r#"codemode exec -- 'python3 -c '\''print(1)'\'''"#);
    }

    #[test]
    fn denylist_passa_cru() {
        // Sem reescrita e sem `allow`: quem decide é a engine do host.
        assert!(claude_(&payload("Bash", "rm -rf /tmp/x")).is_none());
        assert!(claude_(&payload("Bash", "sudo ls")).is_none());
        assert!(claude_(&payload("Bash", "cat .env")).is_none());
    }

    #[test]
    fn nao_embrulha_o_que_ja_esta_roteado() {
        assert!(claude_(&payload("Bash", "codemode exec -- 'ls'")).is_none());
        assert!(claude_(&payload("Bash", "rtk cargo test")).is_none());
    }

    #[test]
    fn wrap_embrulha_por_fora() {
        let s = claude(&payload("Bash", "git status"), Some("'/bin/caveman' shrink --")).unwrap();
        assert_eq!(comando(&s), "'/bin/caveman' shrink -- codemode exec -- 'git status'");
    }

    #[test]
    fn wrap_nao_embrulha_a_si_mesmo() {
        // O agente escreve o programa curto; o --wrap chega com caminho e
        // aspas. Os dois têm que casar, senão vira embrulho de embrulho.
        let w = Some("'/Users/x/.local/bin/caveman' shrink --");
        assert!(claude(&payload("Bash", "caveman shrink -- ls"), w).is_none());
        assert!(claude(&payload("Bash", "'/Users/x/.local/bin/caveman' shrink -- ls"), w).is_none());
    }

    #[test]
    fn wrap_vazio_e_o_mesmo_que_sem_wrap() {
        let s = claude(&payload("Bash", "ls"), Some("   ")).unwrap();
        assert_eq!(comando(&s), "codemode exec -- 'ls'");
    }

    #[test]
    fn ignora_ferramenta_que_nao_e_shell() {
        assert!(claude_(&payload("Read", "git status")).is_none());
    }

    #[test]
    fn entrada_quebrada_nao_derruba_a_chamada() {
        assert!(claude_("nao é json").is_none());
        assert!(claude_("{}").is_none());
        assert!(claude_(&payload("Bash", "   ")).is_none());
    }
}
