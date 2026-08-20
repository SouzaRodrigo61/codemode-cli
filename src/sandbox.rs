//! Filesystem confinement: every path a script touches must resolve to
//! somewhere inside the declared workdir, even through `..`, absolute
//! paths, or symlinks.

use std::path::{Component, Path, PathBuf};

#[derive(Clone)]
pub struct Sandbox {
    /// Modo `--dry-run`: primitiva que mutaria disco ou rodaria comando
    /// apenas anuncia o que faria (#16).
    pub dry: bool,
    /// Segundos que um comando de shell pode rodar antes de ser morto.
    /// 0 = sem limite (#18).
    pub cmd_timeout: u64,
    /// Canonicalized root. All resolved paths must live under this.
    pub root: PathBuf,
    /// Raízes adicionais de `--extra-root` (#4). O ecossistema é multi-repo:
    /// sem isso, toda tarefa que cruza repositório cai fora do codemode.
    /// Cada raiz é confinada individualmente -- não há caminho entre elas.
    pub extra_roots: Vec<PathBuf>,
}

impl Sandbox {
    pub fn with_dry(mut self, dry: bool) -> Self {
        self.dry = dry;
        self
    }

    pub fn with_cmd_timeout(mut self, secs: u64) -> Self {
        self.cmd_timeout = secs;
        self
    }

    /// Cada raiz extra é canonicalizada na entrada: caminho que não existe
    /// não vira raiz, e symlink é resolvido aqui, não na hora de usar.
    pub fn with_extra_roots(mut self, dirs: &[PathBuf]) -> Result<Self, String> {
        for d in dirs {
            let canon = std::fs::canonicalize(d)
                .map_err(|e| format!("extra-root {:?} não existe ou é inacessível: {e}", d))?;
            if !self.extra_roots.contains(&canon) && canon != self.root {
                self.extra_roots.push(canon);
            }
        }
        Ok(self)
    }

    /// Raiz primária primeiro: caminho relativo sempre resolve nela.
    fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.root).chain(self.extra_roots.iter())
    }

    fn dentro_de_alguma(&self, p: &Path) -> bool {
        self.roots().any(|r| p.starts_with(r))
    }

    pub fn new(workdir: &Path) -> Result<Self, String> {
        let root = std::fs::canonicalize(workdir)
            .map_err(|e| format!("workdir {:?} does not exist or is inaccessible: {e}", workdir))?;
        Ok(Sandbox { root, dry: false, cmd_timeout: 600, extra_roots: Vec::new() })
    }

    /// Canonicaliza o ancestral existente mais longo e recoloca o resto do
    /// caminho por cima. Devolve None se nada do caminho existe.
    fn canonicalizar_prefixo(p: &Path) -> Option<PathBuf> {
        let mut existente = p.to_path_buf();
        let mut resto: Vec<std::ffi::OsString> = Vec::new();
        loop {
            if existente.exists() {
                let canon = std::fs::canonicalize(&existente).ok()?;
                let mut out = canon;
                for parte in resto.iter().rev() {
                    out.push(parte);
                }
                return Some(out);
            }
            let nome = existente.file_name()?;
            resto.push(nome.to_os_string());
            if !existente.pop() {
                return None;
            }
        }
    }

    /// Lexically collapse `.` and `..` without touching the filesystem.
    fn normalize_lexical(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for comp in path.components() {
            match comp {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    }

    /// Resolve `input` (relative to the sandbox root, or absolute) to a
    /// path guaranteed to live inside the sandbox. The target need not
    /// exist (needed for `write_file`), but if any existing ancestor is a
    /// symlink pointing outside the sandbox, resolution fails.
    pub fn resolve(&self, input: &str) -> Result<PathBuf, String> {
        let p = Path::new(input);
        let candidate = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        };
        let normalized = Self::normalize_lexical(&candidate);

        if !self.dentro_de_alguma(&normalized) {
            // A raiz pode ter sido declarada por um caminho que é symlink
            // (no macOS, /var -> /private/var): comparar texto com texto
            // recusaria caminho legítimo. Resolver o ancestral existente
            // decide pelo caminho real -- e, por já resolver symlink, é
            // seguro devolver direto.
            if let Some(real) = Self::canonicalizar_prefixo(&normalized) {
                if self.dentro_de_alguma(&real) {
                    return Ok(real);
                }
            }
            return Err(format!(
                "refused: path {:?} resolves outside sandbox workdir {:?}{}",
                input,
                self.root,
                if self.extra_roots.is_empty() {
                    String::new()
                } else {
                    format!(" (e fora das {} raízes extras)", self.extra_roots.len())
                }
            ));
        }

        // Walk up to the longest existing ancestor and canonicalize it, to
        // catch symlinks that point outside the sandbox even though the
        // lexical path looked fine.
        let mut check = normalized.clone();
        loop {
            if check.exists() {
                let canon = std::fs::canonicalize(&check)
                    .map_err(|e| format!("cannot resolve {:?}: {e}", check))?;
                if !self.dentro_de_alguma(&canon) {
                    return Err(format!(
                        "refused: path {:?} escapes sandbox workdir via symlink",
                        input
                    ));
                }
                break;
            }
            if !check.pop() {
                break;
            }
        }

        Ok(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn blocks_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        assert!(sb.resolve("../../etc/passwd").is_err());
    }

    #[test]
    fn blocks_absolute_escape() {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        assert!(sb.resolve("/etc/passwd").is_err());
    }

    #[test]
    fn allows_relative_inside() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        assert!(sb.resolve("a.txt").is_ok());
        assert!(sb.resolve("sub/new.txt").is_ok()); // doesn't exist yet, still fine
    }

    #[test]
    fn extra_root_permite_ler_o_outro_repo_mas_nao_o_resto() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let fora = tempfile::tempdir().unwrap();
        fs::write(b.path().join("outro.txt"), "oi").unwrap();
        let sb = Sandbox::new(a.path()).unwrap().with_extra_roots(&[b.path().to_path_buf()]).unwrap();

        assert!(sb.resolve(b.path().join("outro.txt").to_str().unwrap()).is_ok());
        assert!(sb.resolve(fora.path().join("x.txt").to_str().unwrap()).is_err());
        assert!(sb.resolve("../../etc/passwd").is_err());
    }

    #[test]
    fn symlink_de_uma_raiz_pra_fora_continua_bloqueado_com_extra_root() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let fora = tempfile::tempdir().unwrap();
        fs::write(fora.path().join("segredo.txt"), "nao").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(fora.path(), a.path().join("fuga")).unwrap();
        let sb = Sandbox::new(a.path()).unwrap().with_extra_roots(&[b.path().to_path_buf()]).unwrap();
        #[cfg(unix)]
        assert!(sb.resolve("fuga/segredo.txt").is_err());
    }

    #[test]
    fn blocks_symlink_escape() {
        let outside = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "nope").unwrap();
        let link = dir.path().join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        #[cfg(unix)]
        assert!(sb.resolve("escape/secret.txt").is_err());
    }
}
