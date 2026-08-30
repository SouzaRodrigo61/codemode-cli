//! Liga (e desliga) os hooks deste binário no `settings.json` do host.
//!
//! Mora aqui, e não no `install.sh`, por uma razão prática: mexer em JSON
//! de terceiro em bash exige `jq`, que não está garantido em máquina
//! nenhuma -- e o binário já carrega `serde_json`, além de poder testar o
//! resultado. O instalador só chama.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Marca do que é nosso dentro do settings do host. Não é comentário
/// (JSON não tem) nem chave extra (o host validaria): é o próprio comando,
/// que sempre contém isto. Serve para reconhecer na reinstalação e para
/// remover na desinstalação sem tocar em hook de mais ninguém.
const MARCA: &str = "hook claude";

pub fn caminho_settings() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir).join("settings.json");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".claude").join("settings.json")
}

fn e_nosso(cmd: &str) -> bool {
    cmd.contains(MARCA) && cmd.contains("codemode")
}

fn grupos_mut<'a>(raiz: &'a mut Map<String, Value>, evento: &str) -> &'a mut Vec<Value> {
    raiz.entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("hooks precisa ser objeto")
        .entry(evento)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("evento precisa ser lista")
}

fn comando_de(g: &Value) -> Option<&str> {
    g.get("hooks")?.as_array()?.first()?.get("command")?.as_str()
}

/// Escreve o comando no evento, substituindo o nosso se já existir.
/// Reinstalar não duplica -- e, quando o chamador não passa `--wrap`,
/// preserva a linha que já estava lá: quem encadeia outro roteador não
/// perde o encadeamento por rodar o instalador de novo.
fn garante(raiz: &mut Map<String, Value>, evento: &str, novo: &str, sobrescreve: bool) -> &'static str {
    let grupos = grupos_mut(raiz, evento);
    for g in grupos.iter_mut() {
        let Some(atual) = comando_de(g) else { continue };
        if !e_nosso(atual) {
            continue;
        }
        if atual == novo || !sobrescreve {
            return "já estava";
        }
        g["hooks"][0]["command"] = json!(novo);
        return "atualizado";
    }
    grupos.push(json!({
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": novo }]
    }));
    "ligado"
}

pub struct Plano {
    pub settings: PathBuf,
    pub binario: String,
    pub wrap: Option<String>,
    pub dry_run: bool,
}

/// Caminho absoluto, sempre: sessão spawnada (Remote Control, worker de
/// orquestração, subagente) pode subir com PATH mínimo e perder o hook
/// sem dizer nada.
pub fn binario_absoluto() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "codemode".into())
}

fn le(caminho: &Path) -> Result<Map<String, Value>, String> {
    match std::fs::read_to_string(caminho) {
        Err(_) => Ok(Map::new()),
        Ok(t) if t.trim().is_empty() => Ok(Map::new()),
        Ok(t) => serde_json::from_str::<Value>(&t)
            .map_err(|e| format!("{} não é JSON válido ({e}) -- recusando sobrescrever", caminho.display()))?
            .as_object()
            .cloned()
            .ok_or_else(|| format!("{} não é um objeto JSON", caminho.display())),
    }
}

fn grava(caminho: &Path, raiz: &Map<String, Value>) -> Result<(), String> {
    if let Some(pai) = caminho.parent() {
        let _ = std::fs::create_dir_all(pai);
    }
    if caminho.exists() {
        let bak = caminho.with_extension(format!("json.codemode-bak.{}", crate::hook::agora()));
        let _ = std::fs::copy(caminho, &bak);
    }
    let mut txt = serde_json::to_string_pretty(raiz).map_err(|e| e.to_string())?;
    txt.push('\n');
    std::fs::write(caminho, txt).map_err(|e| e.to_string())
}

pub fn instala(p: &Plano) -> Result<Vec<String>, String> {
    let mut raiz = le(&p.settings)?;
    let pre = match &p.wrap {
        Some(w) => format!("'{}' hook claude --wrap \"{w}\"", p.binario),
        None => format!("'{}' hook claude", p.binario),
    };
    let post = format!("'{}' hook claude --post", p.binario);
    let sobrescreve = p.wrap.is_some();
    let mut linhas = vec![
        format!("PreToolUse  {}", garante(&mut raiz, "PreToolUse", &pre, sobrescreve)),
        format!("PostToolUse {}", garante(&mut raiz, "PostToolUse", &post, true)),
    ];
    if p.dry_run {
        linhas.push("(--dry-run: nada escrito)".into());
        return Ok(linhas);
    }
    grava(&p.settings, &raiz)?;
    linhas.push(format!("settings: {}", p.settings.display()));
    Ok(linhas)
}

pub fn desinstala(caminho: &Path) -> Result<usize, String> {
    let mut raiz = le(caminho)?;
    let mut removidos = 0;
    if let Some(hooks) = raiz.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for (_, grupos) in hooks.iter_mut() {
            let Some(lista) = grupos.as_array_mut() else { continue };
            let antes = lista.len();
            lista.retain(|g| !comando_de(g).is_some_and(e_nosso));
            removidos += antes - lista.len();
        }
        // Evento que ficou vazio some: settings limpo é settings legível.
        hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    }
    if removidos > 0 {
        grava(caminho, &raiz)?;
    }
    Ok(removidos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plano(dir: &Path, wrap: Option<&str>) -> Plano {
        Plano {
            settings: dir.join("settings.json"),
            binario: "/opt/codemode".into(),
            wrap: wrap.map(str::to_string),
            dry_run: false,
        }
    }

    fn comandos(caminho: &Path, evento: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(caminho).unwrap()).unwrap();
        v["hooks"][evento]
            .as_array()
            .map(|a| a.iter().filter_map(|g| comando_de(g).map(str::to_string)).collect())
            .unwrap_or_default()
    }

    #[test]
    fn liga_nos_dois_eventos_e_nao_duplica() {
        let d = tempfile::tempdir().unwrap();
        let p = plano(d.path(), None);
        instala(&p).unwrap();
        instala(&p).unwrap();
        instala(&p).unwrap();
        assert_eq!(comandos(&p.settings, "PreToolUse"), vec!["'/opt/codemode' hook claude"]);
        assert_eq!(comandos(&p.settings, "PostToolUse"), vec!["'/opt/codemode' hook claude --post"]);
    }

    #[test]
    fn preserva_hook_de_terceiro_e_o_wrap_ja_configurado() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("settings.json");
        std::fs::write(&s, r#"{"env":{"X":"1"},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"outro-hook"}]}]}}"#).unwrap();
        // Primeira instalação COM wrap (é o que o operador escolheu).
        instala(&plano(d.path(), Some("'/bin/cv' shrink --"))).unwrap();
        // Reinstalação sem passar wrap não pode apagar a escolha anterior.
        instala(&plano(d.path(), None)).unwrap();
        let pre = comandos(&s, "PreToolUse");
        assert!(pre.contains(&"outro-hook".to_string()), "{pre:?}");
        assert!(pre.iter().any(|c| c.contains("--wrap")), "wrap perdido: {pre:?}");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&s).unwrap()).unwrap();
        assert_eq!(v["env"]["X"], "1", "resto do settings foi mexido");
    }

    #[test]
    fn desinstala_tira_so_o_nosso() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("settings.json");
        std::fs::write(&s, r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"outro-hook"}]}]}}"#).unwrap();
        instala(&plano(d.path(), None)).unwrap();
        assert_eq!(desinstala(&s).unwrap(), 2);
        assert_eq!(comandos(&s, "PreToolUse"), vec!["outro-hook"]);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&s).unwrap()).unwrap();
        assert!(v["hooks"].get("PostToolUse").is_none(), "evento vazio deveria sumir");
        assert_eq!(desinstala(&s).unwrap(), 0, "desinstalar de novo não é erro");
    }

    #[test]
    fn json_quebrado_e_recusa_e_nao_estrago() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("settings.json");
        std::fs::write(&s, "{ não é json").unwrap();
        assert!(instala(&plano(d.path(), None)).is_err());
        assert_eq!(std::fs::read_to_string(&s).unwrap(), "{ não é json");
    }

    #[test]
    fn dry_run_nao_escreve() {
        let d = tempfile::tempdir().unwrap();
        let mut p = plano(d.path(), None);
        p.dry_run = true;
        instala(&p).unwrap();
        assert!(!p.settings.exists());
    }
}
