//! Filesystem confinement: every path a script touches must resolve to
//! somewhere inside the declared workdir, even through `..`, absolute
//! paths, or symlinks.

use std::path::{Component, Path, PathBuf};

#[derive(Clone)]
pub struct Sandbox {
    /// Canonicalized root. All resolved paths must live under this.
    pub root: PathBuf,
}

impl Sandbox {
    pub fn new(workdir: &Path) -> Result<Self, String> {
        let root = std::fs::canonicalize(workdir)
            .map_err(|e| format!("workdir {:?} does not exist or is inaccessible: {e}", workdir))?;
        Ok(Sandbox { root })
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

        if !normalized.starts_with(&self.root) {
            return Err(format!(
                "refused: path {:?} resolves outside sandbox workdir {:?}",
                input, self.root
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
                if !canon.starts_with(&self.root) {
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
