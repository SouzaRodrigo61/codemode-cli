//! Denylist for `run_shell`: blocks the obvious destructive/credential
//! patterns by default. Same spirit as RTK/leanCTX in the Thurion
//! ecosystem — this is NOT a full shell sandbox, just a seatbelt against
//! the most common footguns. A script can only bypass it by explicitly
//! passing `confirm: true`.

pub struct Rule {
    pub name: &'static str,
    matches: fn(&str) -> bool,
}

fn contains(lc: &str, needle: &str) -> bool {
    lc.contains(needle)
}

/// `git push` with a force flag: `--force`, `--force-with-lease`, or a
/// bare ` -f` short flag.
fn is_force_push(lc: &str) -> bool {
    if !lc.contains("git") || !lc.contains("push") {
        return false;
    }
    lc.contains("--force") || lc.contains(" -f") || lc.ends_with("-f")
}

fn is_rm_rf(lc: &str) -> bool {
    // catches `rm -rf`, `rm -fr`, `rm -r -f`, `rm --recursive --force`
    let is_rm_invocation = lc.split_whitespace().any(|w| w == "rm" || w.ends_with("/rm"));
    if !is_rm_invocation {
        return false;
    }
    if lc.contains("-rf") || lc.contains("-fr") {
        return true;
    }
    let has_recursive = lc.split_whitespace().any(|w| w == "-r" || w == "--recursive");
    let has_force = lc.split_whitespace().any(|w| w == "-f" || w == "--force");
    has_recursive && has_force
}

fn is_credential_access(lc: &str) -> bool {
    contains(lc, ".env") || contains(lc, ".ssh") || contains(lc, "id_rsa") || contains(lc, "credentials.json")
}

fn is_fork_bomb(lc: &str) -> bool {
    contains(lc, ":(){ :|:& };:") || contains(lc, ":(){:|:&};:")
}

const RULES: &[Rule] = &[
    Rule { name: "rm -rf (recursive force delete)", matches: is_rm_rf },
    Rule { name: "git push --force", matches: is_force_push },
    Rule { name: "git reset --hard", matches: |lc| contains(lc, "git reset --hard") || contains(lc, "git reset  --hard") },
    Rule { name: "git clean -f", matches: |lc| contains(lc, "git clean") && (contains(lc, "-f") || contains(lc, "--force")) },
    Rule { name: "DROP TABLE", matches: |lc| contains(lc, "drop table") || contains(lc, "drop database") },
    Rule { name: "sudo", matches: |lc| lc.split_whitespace().any(|w| w == "sudo") },
    Rule { name: "credential/secret file access (.env, .ssh, id_rsa, credentials.json)", matches: is_credential_access },
    Rule { name: "fork bomb", matches: is_fork_bomb },
];

/// Returns the name of the first matched denylist rule, if any.
pub fn check(cmd: &str) -> Option<&'static str> {
    let lc = cmd.to_lowercase();
    RULES.iter().find(|r| (r.matches)(&lc)).map(|r| r.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_rm_rf() {
        assert!(check("rm -rf /").is_some());
        assert!(check("rm -rf target").is_some());
    }

    #[test]
    fn blocks_force_push() {
        assert!(check("git push --force origin main").is_some());
        assert!(check("git push -f").is_some());
    }

    #[test]
    fn allows_benign_commands() {
        assert!(check("ls -la").is_none());
        assert!(check("cargo test").is_none());
        assert!(check("git push origin main").is_none());
        assert!(check("git status").is_none());
    }

    #[test]
    fn blocks_credential_access() {
        assert!(check("cat .env").is_some());
        assert!(check("cat ~/.ssh/id_rsa").is_some());
    }

    #[test]
    fn blocks_fork_bomb() {
        assert!(check(":(){ :|:& };:").is_some());
    }
}
