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

/// `None` significa "passa cru": o host segue com o comando original e com
/// a decisão de permissão que ele já tomaria sozinho.
///
/// A denylist é o único portão aqui, e ela devolve `None` de propósito:
/// reescrever `rm -rf` seria pedir `allow` justamente para o que existe
/// para ser perguntado. O que a denylist pega continua chegando cru na
/// engine de permissão do host.
pub fn claude(entrada: &str) -> Option<String> {
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
    let reescrito = format!("codemode exec -- {}", aspas(cmd));
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
        let s = claude(&payload("Bash", "git status")).unwrap();
        assert_eq!(comando(&s), "codemode exec -- 'git status'");
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    }

    #[test]
    fn linha_de_shell_vira_um_argumento_so() {
        // O pipe tem que sobreviver: um argumento só é linha de shell para
        // `exec_one`, dois ou mais seriam argv e o `| head` se perderia.
        let s = claude(&payload("Bash", "git log --oneline | head -5")).unwrap();
        assert_eq!(comando(&s), "codemode exec -- 'git log --oneline | head -5'");
    }

    #[test]
    fn aspas_simples_no_comando_sobrevivem() {
        let s = claude(&payload("Bash", "python3 -c 'print(1)'")).unwrap();
        assert_eq!(comando(&s), r#"codemode exec -- 'python3 -c '\''print(1)'\'''"#);
    }

    #[test]
    fn denylist_passa_cru() {
        // Sem reescrita e sem `allow`: quem decide é a engine do host.
        assert!(claude(&payload("Bash", "rm -rf /tmp/x")).is_none());
        assert!(claude(&payload("Bash", "sudo ls")).is_none());
        assert!(claude(&payload("Bash", "cat .env")).is_none());
    }

    #[test]
    fn nao_embrulha_o_que_ja_esta_roteado() {
        assert!(claude(&payload("Bash", "codemode exec -- 'ls'")).is_none());
        assert!(claude(&payload("Bash", "rtk cargo test")).is_none());
    }

    #[test]
    fn ignora_ferramenta_que_nao_e_shell() {
        assert!(claude(&payload("Read", "git status")).is_none());
    }

    #[test]
    fn entrada_quebrada_nao_derruba_a_chamada() {
        assert!(claude("nao é json").is_none());
        assert!(claude("{}").is_none());
        assert!(claude(&payload("Bash", "   ")).is_none());
    }
}
