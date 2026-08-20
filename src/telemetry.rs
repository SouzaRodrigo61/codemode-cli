//! Uma linha JSON por execução, anexada em `$CODEMODE_HOME/runs.jsonl`
//! (padrão: `~/.codemode/runs.jsonl`).
//!
//! Existe porque o binário não registrava nada sobre si mesmo: a auditoria
//! que originou o roadmap v1.0 teve que ser reconstruída por engenharia
//! reversa dos transcripts do host (issue #11). Duas regras inegociáveis:
//!
//! 1. **Best-effort.** Qualquer falha aqui é engolida — telemetria nunca
//!    pode derrubar uma execução que teria dado certo.
//! 2. **Só metadado.** Nunca o fonte do script, o conteúdo de um arquivo,
//!    a saída de um comando ou os `--arg`. Contagem e tamanho, mais nada.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Entry {
    /// Epoch em segundos.
    pub ts: u64,
    /// Hash do fonte (FNV-1a 64), para agrupar reexecuções do mesmo script
    /// sem nunca guardar o fonte em si.
    pub script: String,
    /// "file" | "lib" | "stdin"
    pub source: String,
    /// Nome do arquivo quando veio de disco; ausente em stdin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Contagem por primitiva, vinda dos próprios pontos de registro do
    /// engine — o que foi de fato despachado, não regex sobre o fonte.
    pub prims: BTreeMap<String, u64>,
    pub prim_total: u64,
    pub out_bytes: u64,
    pub exit_code: i32,
    pub ms: u64,
    pub workdir: String,
}

impl Entry {
    /// Tool-calls que a execução evitou: N primitivas num script custam uma
    /// chamada só, então o que se poupou são as N-1 restantes. Script sem
    /// primitiva nenhuma não poupou nada.
    pub fn calls_avoided(&self) -> u64 {
        self.prim_total.saturating_sub(1)
    }

    pub fn ok(&self) -> bool {
        self.exit_code == 0
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// FNV-1a 64. Não é criptográfico de propósito: serve só para agrupar
/// execuções do mesmo fonte, e evita arrastar uma dependência de hash.
pub fn hash(source: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in source.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn home() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CODEMODE_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    std::env::var("HOME").ok().filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".codemode"))
}

pub fn log_path() -> Option<PathBuf> {
    home().map(|h| h.join("runs.jsonl"))
}

/// Anexa a entrada. Silencioso em qualquer falha, por desenho.
pub fn record(entry: &Entry) {
    if std::env::var("CODEMODE_NO_TELEMETRY").is_ok() {
        return;
    }
    let Some(path) = log_path() else { return };
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(line) = serde_json::to_string(entry) else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Lê o log inteiro, descartando linha corrompida em silêncio — um log
/// meio escrito não pode quebrar o relatório.
pub fn load() -> Vec<Entry> {
    let Some(path) = log_path() else { return Vec::new() };
    let Ok(raw) = std::fs::read_to_string(path) else { return Vec::new() };
    raw.lines().filter_map(|l| serde_json::from_str::<Entry>(l).ok()).collect()
}
