#!/usr/bin/env bash
# codemode installer -- builds the binary, puts it on PATH, and wires the
# "prefer codemode for multi-step work" hint into every agent CLI's context
# file it finds on this machine. Idempotent: safe to re-run after a git
# pull to pick up a new build, never duplicates the hint line.
#
# Deliberately does not assume which CLIs are installed -- it *detects*
# them by checking for each one's known context-file location, the same
# way `maestri` bindings auto-detect at runtime (see src/maestri.rs). A
# machine with none of the 4 known CLIs still gets a working `codemode`
# binary on PATH; a machine with a CLI this script doesn't know about yet
# gets that binary too, just without the auto-wired hint (fixable by
# adding one entry to CONTEXT_FILES below).
set -euo pipefail

BOLD='\033[1m'; DIM='\033[2m'; GREEN='\033[32m'; RESET='\033[0m'
say() { printf "${BOLD}%s${RESET}\n" "$1"; }
info() { printf "  ${DIM}%s${RESET}\n" "$1"; }
ok() { printf "  ${GREEN}✓${RESET} %s\n" "$1"; }

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${CODEMODE_INSTALL_DIR:-$HOME/.local/bin}"
# CODEMODE_CONTEXT_ROOT overrides $HOME for the four paths below, DESTDIR-
# style -- lets `./install.sh` be smoke-tested end to end (including the
# context-file wiring step) against a scratch directory instead of a
# developer's real dotfiles. Unset (the normal case) means "wire the real
# CLIs on this machine."
CONTEXT_ROOT="${CODEMODE_CONTEXT_ROOT:-$HOME}"
HINT_SECTION_HEADER='## Code Mode — prefer it for multi-step file/shell work'
HINT_LINE='When doing 3+ sequential file/shell operations, write a .rhai script and run `codemode run script.rhai` via Bash instead of separate Read/Edit/Bash calls. See `codemode run --help`.'

# "Name|context file" pairs, one per known CLI. Add a line here to teach
# the installer about a new one; nothing else in this script changes.
# (Plain array, not associative -- macOS ships bash 3.2 by default, which
# predates `declare -A`; this has to work with zero setup on a stock Mac.)
CONTEXT_FILES=(
  "Claude Code|$CONTEXT_ROOT/.claude/CLAUDE.md"
  "Codex|$CONTEXT_ROOT/.codex/AGENTS.md"
  "Grok Build|$CONTEXT_ROOT/.grok/AGENTS.md"
  "OpenCode|$CONTEXT_ROOT/.config/opencode/AGENTS.md"
)

say "1/4 — building codemode"
cd "$REPO_DIR"
if ! command -v cargo >/dev/null 2>&1; then
  echo "codemode install: cargo not found. Install Rust (https://rustup.rs) and re-run." >&2
  exit 1
fi
cargo build --release --quiet
ok "built target/release/codemode"

say "2/4 — installing to PATH"
mkdir -p "$INSTALL_DIR"
# rm antes do cp: sobrescrever um binário já mapeado invalida a assinatura
# ad-hoc e o macOS mata o processo seguinte com SIGKILL, sem mensagem que
# explique nada. Inode novo + re-assinatura resolve, e em Linux é inócuo.
rm -f "$INSTALL_DIR/codemode"
cp target/release/codemode "$INSTALL_DIR/codemode"
chmod +x "$INSTALL_DIR/codemode"
if command -v codesign >/dev/null 2>&1; then
  codesign -f -s - "$INSTALL_DIR/codemode" >/dev/null 2>&1 || true
fi
ok "copied to $INSTALL_DIR/codemode"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ok "$INSTALL_DIR is already on PATH" ;;
  *) info "⚠ $INSTALL_DIR is not on your PATH — add it in your shell profile:"
     info "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

say "3/4 — wiring detected agent CLIs"
found_any=false
for pair in "${CONTEXT_FILES[@]}"; do
  name="${pair%%|*}"
  file="${pair#*|}"
  dir="$(dirname "$file")"
  if [ ! -d "$dir" ]; then
    info "$name — not detected (no $dir), skipped"
    continue
  fi
  found_any=true
  mkdir -p "$dir"
  touch "$file"
  # Match on the section header, not the exact hint sentence -- the wording
  # has already been hand-edited per-CLI once (see git history) and will
  # likely be again; checking the header is what makes re-running this
  # script safe without clobbering or duplicating a manually-tuned hint.
  if grep -qF "$HINT_SECTION_HEADER" "$file" 2>/dev/null; then
    ok "$name — already wired ($file)"
  else
    printf '\n%s\n\n%s\n' "$HINT_SECTION_HEADER" "$HINT_LINE" >> "$file"
    ok "$name — wired ($file)"
  fi
done
if [ "$found_any" = false ]; then
  info "no known agent CLI context directories found on this machine — binary is"
  info "still installed and usable, just not auto-wired anywhere"
fi

say "4/4 — wiring the shell hooks (Claude Code)"
# O binário faz a edição, não este script: mexer em JSON de terceiro em
# bash exigiria `jq`, que não está garantido em máquina nenhuma. Aqui só
# se decide SE roda -- e o passo é o único do instalador que muda como
# toda chamada de shell do host se comporta, então tem porta de saída.
CLAUDE_DIR="$CONTEXT_ROOT/.claude"
if [ -n "${CODEMODE_NO_HOOKS:-}" ]; then
  info "CODEMODE_NO_HOOKS set — skipped"
elif [ ! -d "$CLAUDE_DIR" ]; then
  info "Claude Code not detected (no $CLAUDE_DIR), skipped"
else
  # Sem --wrap de propósito: encadeamento é escolha da máquina, e reinstalar
  # preserva o que já estiver configurado lá.
  "$INSTALL_DIR/codemode" hooks install claude --settings "$CLAUDE_DIR/settings.json" \
    | while IFS= read -r line; do ok "$line"; done
  info "every Bash call now routes through \`codemode exec\` (rtk filter, denylist,"
  info "output cap, telemetry) and a burst of 5+ commands gets one reminder"
  info "undo with: codemode hooks uninstall claude"
fi

echo
say "Done."
"$INSTALL_DIR/codemode" run - --workdir /tmp <<'RHAI' >/dev/null
print("smoke test ok");
RHAI
ok "smoke test passed — codemode is working"
