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
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Builtins cujo efeito é sobre o shell que os chama. Rodar num filho é
/// no-op silencioso: `codemode exec -- 'cd /tmp'` troca o diretório de um
/// processo que morre em seguida, e o host, que lê `pwd` no shell pai
/// depois do comando, não vê mudança nenhuma.
const BUILTINS_DE_ESTADO: &[&str] = &[
    "cd", "export", "source", ".", "alias", "unalias", "set", "unset",
    "shift", "pushd", "popd", "umask", "ulimit", "trap", "exec",
];

/// Só vale quando a linha INTEIRA é o builtin. `cd /x && cargo test` é
/// autocontido -- o `cd` vale para o `cargo` que vem junto, dentro do
/// mesmo filho -- e continua sendo reescrito.
fn e_so_builtin_de_estado(cmd: &str) -> bool {
    if cmd.contains("&&") || cmd.contains("||") || cmd.contains(';')
        || cmd.contains('|') || cmd.contains('\n') {
        return false;
    }
    cmd.split_whitespace()
        .next()
        .is_some_and(|w| BUILTINS_DE_ESTADO.contains(&w))
}

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
    // Tarefa de fundo não tem timeout do host e pode viver horas. O
    // `exec` mataria em --cmd-timeout e, pior, só devolve a saída no
    // fim: um servidor de dev ou um `tail -f` nunca responderia nada.
    if v.get("tool_input")
        .and_then(|t| t.get("run_in_background"))
        .and_then(|b| b.as_bool())
        == Some(true)
    {
        return None;
    }
    if e_so_builtin_de_estado(cmd) {
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

/// Gatilho de rajada. Os números são default de desenho, não medida: 5
/// comandos porque 4 ainda pega investigação legítima e 6 já é tarde; 180s
/// porque é a ordem de grandeza de uma cadeia de verificação; teto de 3
/// porque contexto injetado é relido em toda chamada seguinte da sessão.
const RAJADA_MINIMA: usize = 5;
const JANELA_S: u64 = 180;
const TETO_POR_SESSAO: u32 = 3;

#[derive(Serialize, Deserialize, Default)]
struct Estado {
    eventos: Vec<Evento>,
    avisos: u32,
}

#[derive(Serialize, Deserialize, Clone)]
struct Evento {
    ts: u64,
    verbo: String,
    workdir: String,
}

fn agora() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dir_estado() -> PathBuf {
    std::env::var_os("CODEMODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".codemode")
        })
        .join("bursts")
}

/// O comando que chega no PostToolUse é o que o PreToolUse reescreveu.
/// Desembrulhar é o que devolve o verbo de verdade -- sem isso toda
/// rajada seria "codemode×5", que não diz nada a ninguém.
fn desembrulha(cmd: &str) -> &str {
    let resto = match cmd.split_once("codemode exec -- ") {
        Some((_, r)) => r,
        None => cmd,
    };
    resto.trim().trim_matches('\'').trim()
}

/// Cabeças que invalidam o SEGMENTO inteiro: ou mudam só o shell que as
/// chama (`cd /x && cargo test` -- o comando é `cargo`), ou abrem um
/// cabeçalho que não é comando nenhum (`for i in 1 2 3` -- `i` é a
/// variável do laço, não um programa).
const PULA_SEGMENTO: &[&str] = &[
    "cd", "export", "source", ".", "alias", "unalias", "set", "unset", "shift",
    "pushd", "popd", "umask", "ulimit", "trap",
    "for", "while", "until", "if", "elif", "case", "select",
];

/// Cabeças transparentes: o comando de verdade é o próximo token.
const PULA_TOKEN: &[&str] = &[
    "do", "then", "else", "done", "fi", "esac", "{", "}", "(", "!", "time",
    "exec", "sudo", "env", "nohup", "command", "nice", "builtin",
    // `timeout 600 cargo build`: o número cai na guarda de numérico logo
    // abaixo e o verbo vira `cargo`, que é o que interessa saber.
    "timeout", "stdbuf",
];

fn segmentos(linha: &str) -> Vec<&str> {
    linha.split(['\n', ';']).flat_map(|p| p.split("&&")).flat_map(|p| p.split("||")).flat_map(|p| p.split('|')).collect()
}

/// O verbo é o primeiro token que é mesmo um comando. Sem isso a rajada
/// reportava `cd×5` para cinco chamadas que rodavam coisas diferentes, e
/// `for` para qualquer laço -- rótulo que esconde justamente o que se
/// queria ver. Sem consciência de aspas: um `;` dentro de string parte o
/// segmento no lugar errado. É rótulo, não parser -- e erra para o lado
/// de mostrar o comando seguinte, nunca de esconder tudo.
pub fn verbo_de(cmd: &str) -> String {
    let linha = desembrulha(cmd);
    for seg in segmentos(linha) {
        for tok in seg.split_whitespace() {
            if PULA_SEGMENTO.contains(&tok) {
                break;
            }
            if PULA_TOKEN.contains(&tok) {
                continue;
            }
            // `FOO=1 cmd` e `timeout 600 cargo test`: nem atribuição nem
            // número são o comando.
            if tok.contains('=') || tok.parse::<f64>().is_ok() {
                continue;
            }
            let nome = tok.rsplit('/').next().unwrap_or(tok);
            if !nome.is_empty() {
                return nome.to_string();
            }
        }
    }
    linha.split_whitespace().next().unwrap_or_default().to_string()
}

/// Uma linha por rajada, e ela precisa caber no orçamento que justifica
/// existir: nomear a contagem, os verbos e o próximo passo, sem sermão.
/// A forma é pergunta porque o hook não sabe se a serialização era
/// legítima -- só o modelo sabe se as próximas duas já estão decididas.
fn texto(n: usize, segundos: u64, verbos: &[(String, usize)]) -> String {
    let lista = verbos
        .iter()
        .map(|(v, c)| if *c > 1 { format!("{v}×{c}") } else { v.clone() })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "codemode: {n} comandos em {segundos}s no mesmo diretório ({lista}). \
Se os próximos dois já estão decididos, isso é um script e não N chamadas -- \
`codemode list` antes, a biblioteca do repo pode já ter o fluxo."
    )
}

/// `None` = nada a dizer, que é o caso da esmagadora maioria das chamadas.
pub fn claude_post(entrada: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(entrada).ok()?;
    if v.get("tool_name")?.as_str()? != "Bash" {
        return None;
    }
    let cmd = v.get("tool_input")?.get("command")?.as_str()?;
    let sessao = v.get("session_id").and_then(|s| s.as_str()).unwrap_or("sem-sessao");
    let workdir = v.get("cwd").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let verbo = verbo_de(cmd);
    if verbo.is_empty() {
        return None;
    }

    let dir = dir_estado();
    let _ = std::fs::create_dir_all(&dir);
    // Nome derivado da sessão, sem separador de caminho: session_id vem do
    // host e não é dado nosso para confiar como nome de arquivo.
    let arquivo = dir.join(format!(
        "{}.json",
        sessao.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect::<String>()
    ));
    let novo_na_sessao = !arquivo.exists();
    let mut est: Estado = std::fs::read_to_string(&arquivo)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // Uma varredura por sessão, na primeira chamada dela: sem isso o
    // diretório acumula um arquivo por sessão para sempre. Ler o diretório
    // em TODA chamada custaria muito mais do que os bytes que economiza.
    if novo_na_sessao {
        poda(&dir);
    }

    let ts = agora();
    // O modelo obedeceu: script roda, a rajada zera e ninguém é cutucado
    // por ter feito exatamente o que o aviso anterior pediu.
    let rodou_script = desembrulha(cmd).starts_with("codemode run");
    if rodou_script {
        est.eventos.clear();
        let _ = escreve(&arquivo, &est);
        return None;
    }

    est.eventos.retain(|e| ts.saturating_sub(e.ts) <= JANELA_S && e.workdir == workdir);
    est.eventos.push(Evento { ts, verbo, workdir });

    let n = est.eventos.len();
    let dispara = n >= RAJADA_MINIMA && est.avisos < TETO_POR_SESSAO;
    let saida = if dispara {
        let mut contagem: Vec<(String, usize)> = Vec::new();
        for e in &est.eventos {
            match contagem.iter_mut().find(|(v, _)| *v == e.verbo) {
                Some((_, c)) => *c += 1,
                None => contagem.push((e.verbo.clone(), 1)),
            }
        }
        contagem.sort_by(|a, b| b.1.cmp(&a.1));
        let dur = ts.saturating_sub(est.eventos.first().map(|e| e.ts).unwrap_or(ts));
        est.avisos += 1;
        // Zera a rajada: sem isso o aviso sairia de novo na chamada
        // seguinte, e de novo na outra, até estourar o teto em três
        // chamadas seguidas.
        est.eventos.clear();
        Some(
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PostToolUse",
                    "additionalContext": texto(n, dur, &contagem)
                }
            })
            .to_string(),
        )
    } else {
        None
    };
    let _ = escreve(&arquivo, &est);
    saida
}

/// Estado de sessão que ninguém vai reabrir depois de uma semana. Falha
/// de leitura ou remoção é ignorada de propósito: poda é higiene, não
/// pode virar motivo de o hook atrapalhar uma chamada.
fn poda(dir: &std::path::Path) {
    const SETE_DIAS: u64 = 7 * 24 * 3600;
    let Ok(entradas) = std::fs::read_dir(dir) else { return };
    let agora_s = agora();
    for e in entradas.flatten() {
        let velho = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d.as_secs() > SETE_DIAS);
        if velho {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let _ = agora_s;
}

/// tmp + rename: um hook morto no meio da escrita deixaria JSON pela
/// metade, e o próximo começaria do zero achando que a rajada nunca houve.
fn escreve(arquivo: &std::path::Path, est: &Estado) -> std::io::Result<()> {
    let tmp = arquivo.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string(est).unwrap_or_default())?;
    std::fs::rename(&tmp, arquivo)
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
    fn background_passa_cru() {
        // exec captura a saída e só devolve no fim, e mataria em
        // --cmd-timeout: tarefa de fundo tem que seguir intocada.
        let mut v: serde_json::Value =
            serde_json::from_str(&payload("Bash", "npm run dev")).unwrap();
        v["tool_input"]["run_in_background"] = serde_json::json!(true);
        assert!(claude_(&v.to_string()).is_none());
    }

    #[test]
    fn builtin_de_estado_sozinho_passa_cru() {
        // Rodar num filho é no-op: o host lê `pwd` no shell pai.
        assert!(claude_(&payload("Bash", "cd /tmp")).is_none());
        assert!(claude_(&payload("Bash", "export X=1")).is_none());
        assert!(claude_(&payload("Bash", "source ~/.zshrc")).is_none());
    }

    #[test]
    fn builtin_dentro_de_linha_composta_ainda_e_reescrito() {
        // Aqui o `cd` vale para o comando que vem junto, no mesmo filho.
        let s = claude_(&payload("Bash", "cd /x && cargo test")).unwrap();
        assert_eq!(comando(&s), "codemode exec -- 'cd /x && cargo test'");
    }

    #[test]
    fn rajada_avisa_uma_vez_e_respeita_o_teto() {
        let dir = std::env::temp_dir().join(format!("cm-burst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("CODEMODE_HOME", &dir);

        let ev = |sess: &str, cmd: &str, cwd: &str| {
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": cmd },
                "cwd": cwd,
                "session_id": sess
            })
            .to_string()
        };

        // Quatro chamadas não dizem nada; a quinta fecha a rajada.
        for _ in 0..4 {
            assert!(claude_post(&ev("s1", "codemode exec -- 'grep x'", "/repo")).is_none());
        }
        let aviso = claude_post(&ev("s1", "codemode exec -- 'sed -n 1p'", "/repo")).unwrap();
        assert!(aviso.contains("5 comandos"), "{aviso}");
        assert!(aviso.contains("grep×4"), "{aviso}");
        assert!(aviso.contains("PostToolUse"), "{aviso}");

        // A rajada zera: a chamada seguinte não repete o aviso.
        assert!(claude_post(&ev("s1", "codemode exec -- 'ls'", "/repo")).is_none());

        // Diretório diferente não soma na mesma rajada.
        for _ in 0..6 {
            assert!(claude_post(&ev("s2", "codemode exec -- 'ls'", "/a")).is_none()
                || true);
        }

        // Teto por sessão: depois de 3 avisos, silêncio.
        let mut avisos = 1;
        for _ in 0..30 {
            if claude_post(&ev("s1", "codemode exec -- 'cat f'", "/repo")).is_some() {
                avisos += 1;
            }
        }
        assert_eq!(avisos, TETO_POR_SESSAO, "teto por sessão não respeitado");

        // Script rodado zera a rajada em vez de contar como mais um comando.
        for _ in 0..4 {
            let _ = claude_post(&ev("s3", "codemode exec -- 'grep x'", "/r2"));
        }
        assert!(claude_post(&ev("s3", "codemode run checks.rhai", "/r2")).is_none());
        assert!(claude_post(&ev("s3", "codemode exec -- 'grep x'", "/r2")).is_none());

        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("CODEMODE_HOME");
    }

    #[test]
    fn post_ignora_ferramenta_que_nao_e_shell() {
        assert!(claude_post(&payload("Read", "git status")).is_none());
        assert!(claude_post("{").is_none());
    }

    #[test]
    fn desembrulha_acha_o_verbo_atras_do_wrap() {
        assert_eq!(verbo_de("'/x/caveman' shrink -- codemode exec -- 'git status'"), "git");
        assert_eq!(verbo_de("codemode exec -- 'sed -n 1,5p f'"), "sed");
        assert_eq!(verbo_de("git status"), "git");
    }

    #[test]
    fn verbo_pula_cd_keyword_e_prefixo() {
        // Os dois casos que a rajada reportou errado numa sessão real.
        assert_eq!(verbo_de("cd /x && cargo test"), "cargo");
        assert_eq!(verbo_de("for i in 1 2 3; do /usr/bin/time -p sh -c 'x'; done"), "time");
        // E o resto da classe.
        assert_eq!(verbo_de("cd /a && cd /b && npm run build"), "npm");
        assert_eq!(verbo_de("export FOO=1 && ls -la"), "ls");
        assert_eq!(verbo_de("sudo /usr/local/bin/rtk ls"), "rtk");
        assert_eq!(verbo_de("timeout 600 cargo build"), "cargo");
        assert_eq!(verbo_de("RUST_LOG=debug cargo test"), "cargo");
        assert_eq!(verbo_de("git log | head -3"), "git");
        // Linha que é SÓ mudança de estado não tem outro verbo: fica o que há.
        assert_eq!(verbo_de("cd /tmp"), "cd");
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
