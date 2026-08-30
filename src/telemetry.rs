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
    /// Verbo de shell -> contagem (`git`, `cargo`, `make`). Só a primeira
    /// palavra: é o que responde "o que merece virar primitiva nativa?" sem
    /// carregar argumento nenhum (#82).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prims_shell: BTreeMap<String, u64>,
    pub prim_total: u64,
    pub out_bytes: u64,
    pub exit_code: i32,
    pub ms: u64,
    pub workdir: String,
    /// "real" | "bench" | "self" | "check" | "desconhecido". Ausente nas
    /// linhas gravadas antes do #59 -- `kind()` reclassifica essas pela
    /// mesma regra, para que o histórico já escrito não fique de fora.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
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

    /// O `kind` gravado, ou a reclassificação pela mesma regra quando a
    /// linha é anterior ao #59. Nunca falha.
    pub fn kind(&self) -> String {
        if let Some(k) = &self.kind {
            return k.clone();
        }
        classify(&self.workdir, self.name.as_deref())
    }

    /// Trabalho de produto -- o único número que responde "vale a pena?".
    pub fn is_real(&self) -> bool {
        self.kind() == "real"
    }
}

/// Classifica uma execução sem depender de configuração do usuário.
///
/// Existe porque `codemode gain` somava tudo: na máquina onde o #59 foi
/// medido, 1.303 das 1.312 execuções gravadas eram o próprio codemode se
/// desenvolvendo e medindo, contra 9 de trabalho real -- o relatório
/// inflava o ganho ~145x e escondia uma taxa de falha de 33%.
///
/// As três regras são gerais, não uma lista de caminhos desta máquina:
///
/// * `bench/...` no nome do script é caso de benchmark, em qualquer repo.
/// * diretório temporário é rascunho, não trabalho que alguém vai versionar
///   -- **menos** o scratchpad do agente, que é onde o host manda o agente
///   trabalhar e é trabalho de produto como qualquer outro.
/// * o codemode se desenvolvendo é detectado pelo `Cargo.toml` do próprio
///   workdir declarar `name = "codemode"` -- nenhum caminho fixo envolvido.
///
/// Linha antiga cujo workdir não existe mais vira **"desconhecido"**, não
/// "real": das 36 que a primeira versão desta função deu como reais na
/// máquina do #59, 27 eram worktrees de desenvolvimento do próprio codemode
/// já apagadas. Sem o diretório não há como provar, e chutar para o lado
/// "real" infla exatamente o número que esta issue existe para consertar.
/// Execução nova sempre tem workdir vivo, então nasce classificada.
pub fn classify(workdir: &str, name: Option<&str>) -> String {
    if let Some(n) = name {
        let n = n.replace('\\', "/");
        if n.starts_with("bench/") || n.contains("/bench/") {
            return "bench".into();
        }
        // A origem do SCRIPT decide, nao so o workdir. Um benchmark rodado
        // com `--workdir <repo real>` -- que e como se mede qualquer coisa
        // util -- entrava como trabalho e inflava justamente o bucket de
        // desperdicio: 41 execucoes de bench numa unica sessao apareceram
        // como "1 primitiva, use Bash direto".
        // O caminho do script vem como o chamador digitou (`/tmp/x.rhai`),
        // enquanto as raízes são canonicalizadas (`/private/tmp` no macOS):
        // canonicaliza o ancestral antes de comparar, senão a regra vale no
        // Linux e não vale aqui.
        let canon = std::path::Path::new(&n)
            .parent()
            .and_then(|d| std::fs::canonicalize(d).ok())
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| n.clone());
        if (em_temporario(&canon) || em_temporario(&n)) && !e_scratchpad_de_agente(&canon) {
            return "bench".into();
        }
    }
    if em_temporario(workdir) && !e_scratchpad_de_agente(workdir) {
        return "bench".into();
    }
    if e_o_proprio_codemode(workdir) {
        return "self".into();
    }
    if !std::path::Path::new(workdir).is_dir() {
        return "desconhecido".into();
    }
    "real".into()
}

/// O host dá ao agente um diretório de trabalho dentro do temporário do
/// sistema -- e o que acontece lá é trabalho de produto, não rascunho de
/// benchmark. Sem esta exceção o relatório que existe para responder "vale
/// a pena?" fica cego justamente para o agente que mais roda comando: numa
/// medição real, 22 execuções de um posto de trabalho inteiro caíram no
/// balde de excluídos junto com bench.
///
/// A regra é o nome do componente, não um caminho desta máquina: `scratchpad`
/// é como os hosts chamam essa pasta, e a comparação é por componente para
/// que `/tmp/scratchpad-velho` não entre por acidente de prefixo.
fn e_scratchpad_de_agente(caminho: &str) -> bool {
    std::path::Path::new(caminho)
        .components()
        .any(|c| c.as_os_str() == "scratchpad")
}

fn em_temporario(workdir: &str) -> bool {
    let mut raizes =
        vec![String::from("/tmp"), String::from("/private/tmp"), String::from("/var/tmp")];
    if let Ok(t) = std::env::var("TMPDIR") {
        if !t.is_empty() {
            raizes.push(t.trim_end_matches('/').to_string());
        }
    }
    // Canonicaliza os dois lados: o workdir gravado já vem canonicalizado, e
    // no macOS /tmp e TMPDIR resolvem para /private/... -- comparar cru fazia
    // a regra valer no Linux e não valer no macOS.
    raizes
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .map(|r| r.display().to_string())
        .any(|r| workdir == r || workdir.starts_with(&format!("{r}/")))
}

/// `true` quando o workdir (ou um ancestral) é o crate do próprio codemode.
/// Só responde sobre diretório que existe; quem chama trata o sumido como
/// "desconhecido", em vez de deixar a ausência de prova virar prova.
fn e_o_proprio_codemode(workdir: &str) -> bool {
    let mut dir = std::path::Path::new(workdir);
    for _ in 0..6 {
        if let Ok(txt) = std::fs::read_to_string(dir.join("Cargo.toml")) {
            for linha in txt.lines() {
                let l = linha.trim();
                if l.starts_with("name") {
                    return l.contains("\"codemode\"");
                }
            }
        }
        match dir.parent() {
            Some(pai) => dir = pai,
            None => break,
        }
    }
    false
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

#[cfg(test)]
mod testes_classificacao {
    use super::*;

    #[test]
    fn script_em_tmp_e_bench_mesmo_com_workdir_real() {
        // A regressão de medição: benchmark rodado com `--workdir <repo real>`
        // -- que é como se mede qualquer coisa útil -- entrava como trabalho e
        // inflava justamente o bucket de desperdício do `gain`.
        let tmp = std::env::temp_dir().join("bench-x.rhai");
        assert_eq!(classify("/Users/alguem/repo", Some(tmp.to_str().unwrap())), "bench");
    }

    #[test]
    fn script_fora_de_tmp_com_workdir_real_continua_real() {
        let d = std::env::current_dir().unwrap();
        let s = d.join("nao-existe-mas-nao-e-tmp.rhai");
        assert_ne!(classify(d.to_str().unwrap(), Some(s.to_str().unwrap())), "bench");
    }
}
