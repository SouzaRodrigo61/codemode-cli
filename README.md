# codemode

A standalone Rust CLI implementing "code mode" / programmatic tool
calling: the pattern Anthropic measured at ~37% token reduction,
Cloudflare calls "Code Mode", and DeepSeek Harness runs as its default
mode. Instead of an agent doing one tool-call per operation (Read, then
Edit, then Bash, then Grep — each a separate turn), it writes **one
script** that runs in a sandbox calling several primitives in sequence,
and only the consolidated result comes back into the model's context.

## What this is not

Not an MCP server. Not a resident process. No `@modelcontextprotocol/sdk`,
no protocol handshake, nothing to register. It is a compiled binary you
invoke via `Bash`/`exec`, same as [`rtk`](https://github.com/SouzaRodrigo61/rtk): it runs, does the work, exits, frees memory.

Not tied to any workspace or agent product either. The primitives are plain
file and shell operations with no dependency on Claude Code, Codex, Grok
Build, OpenCode, Maestri or Cursor. Integration with a specific external
tool (currently only Maestri) lives in its own module and is registered
into a script's namespace **only when that tool is detected on the
machine**, checked at every invocation — so the same binary in a Cursor
project simply doesn't expose `maestri_*`, because there is nothing there
to shell out to.

## Host-agnostic, and what follows from it

Today Orca, tomorrow Cursor, then a Devin sandbox, then super.engineering,
then just the Claude Code terminal. The binary doesn't care: it's on
`PATH`, you invoke it, it exits.

That portability has a consequence worth stating, because it is easy to
get wrong: **the knowledge about codemode has to live in the binary, not
in the host's context files.**

```bash
codemode            # what it does and which subcommands exist
codemode idioms     # syntax, primitives, limits, traps — always current
codemode list       # this repo's library
```

A skill, a `CLAUDE.md`, an `AGENTS.md` or a persona prompt is specific to
one host and gets left behind when you switch. Worse, any of them that
*copies* the API surface goes stale and starts lying. That has happened
three times in this project: persona prompts carrying the pre-throw
`run_shell` contract, a local gate advertising a `clippy` check it never
ran, and the same outdated timeout rule sitting in 28 files under three
different phrasings.

So: host files may **point** at `codemode idioms`; they must not mirror
it. And if you change a primitive's contract, update `idioms` in the same
PR — a test fails if the sheet stops mentioning the behaviours that change
how a script must be written.

## How it works

- The embedded script language is [Rhai](https://rhai.rs) — a pure-Rust
  scripting engine, no external runtime (no Node/Python/Deno).
- A small set of native Rust functions is registered into the Rhai
  engine: `read_file`, `write_file`, `edit_file`, `run_shell`, `grep`,
  `glob`.
- The agent writes a `.rhai` script and runs it with one `Bash`
  tool-call: `codemode run script.rhai`. That's the whole point — it
  reuses the `Bash` tool every CLI agent already has, instead of adding a
  new tool.

```
codemode run <script.rhai> [--workdir DIR] [--timeout SECS] [--verbose]
codemode run -                          # read the script from stdin
codemode run x.rhai --arg 77 --arg foo  # available as the ARGS array
codemode check x.rhai                   # pre-flight, without running
codemode run x.rhai --dry-run           # announce every write and command, do none
codemode run x.rhai --strict            # refuse a script collapsing fewer than 2 primitives
```

Two output limits, with different jobs:

```
--max-output BYTES    hard cap, 1 MiB. Truncates, and spills the full
                      stream to a temp file named in the notice.
--max-context BYTES   soft threshold, 64 KiB. Warns on stderr and names
                      the largest single print; never truncates.
```

## Install

```bash
./install.sh
```

Builds the release binary, puts it on `PATH` (`~/.local/bin` by default,
override with `CODEMODE_INSTALL_DIR`), auto-detects which of Claude Code /
Codex / Grok Build / OpenCode are present on the machine by checking each
one's known context-file location, and wires the one-line "prefer codemode
for multi-step work" hint into whichever it finds — nothing to configure by
hand, nothing to do twice. Safe to re-run any time (after `git pull`, say):
it recognizes an already-wired file by its section header and never
duplicates the hint, and a CLI it doesn't detect is skipped without error.
Doesn't touch `~/.claude` et al. when `CODEMODE_CONTEXT_ROOT` is set to a
different path — that's how the script tests itself end to end against a
scratch directory instead of a developer's real dotfiles.

Manual install, if you'd rather:

```bash
cargo build --release
cp target/release/codemode ~/.local/bin/   # or anywhere on PATH
codemode run - --workdir . <<'RHAI'
print("codemode is installed");
RHAI
```

Same idea as `rtk`: one static binary on `PATH`, no daemon, no config
file required to start using it.

## Using it from an agent CLI

`codemode` is not MCP, so there is nothing to "register". Installation
is: binary on `PATH`, plus (optionally) one line in whatever context
file each CLI loads at session start, telling the agent to prefer a
`.rhai` script over a chain of separate tool-calls.

| CLI | Context file | Example line to add |
|---|---|---|
| Claude Code | `CLAUDE.md` | `When doing 3+ sequential file/shell operations, write a .rhai script and run \`codemode run script.rhai\` via Bash instead of separate Read/Edit/Bash calls.` |
| Codex | `AGENTS.md` (OpenAI Codex CLI convention) | same line |
| Grok Build | `AGENTS.md` (xAI's Grok Build follows the same `AGENTS.md` convention as Codex/OpenCode) | same line |
| OpenCode | `AGENTS.md` (OpenCode explicitly adopted the `AGENTS.md` convention) | same line |

`AGENTS.md` is the emerging cross-tool convention for "instructions an
agent reads at the start of a session" — Codex, OpenCode, and several
other CLIs read it; Claude Code uses `CLAUDE.md` for the same purpose
(and will also pick up `AGENTS.md` if present, depending on
configuration). None of this requires touching each tool's internals —
it's a one-line documentation change plus the binary being on `PATH`.

## RTK lives inside codemode now, not next to it

`run_shell` had two tiers already (a routing allowlist, `rtk`-worth-it commands only) —
now it has three. For commands with a migrated filter (currently: `cargo test`), codemode
calls `rtk::filters::cargo_test` **in-process**, as a real dependency on
[`thurionapp/rtk`](https://github.com/SouzaRodrigo61/rtk) (`[lib]` target added specifically
for this), instead of spawning the `rtk` binary at all. Measured: the pure filtering step
went from **5.31ms (spawn `rtk`, pipe through `rtk pipe -f cargo-test`) to 0.0055ms
in-process — ~965× faster** for the filtering itself (`cargo run --release --example
measure_inprocess_filter`, isolated from the underlying tool's own runtime, which for
`cargo test` dominates total wall-clock regardless — the win here is real for anything
whose own execution is fast, like `git log`/`git diff`/`git status`, not necessarily
visible on a slow build/test command).

**Real cost, not hidden:** the binary grew from 3.7MB to 5.4MB (+46%) pulling in `rtk`'s
transitive dependencies (its analytics/telemetry stack — `rusqlite`, `chrono`, unicode
tables — none of which codemode itself needs) for one function. Worth it for `cargo test`
specifically (high-value, real production usage); worth watching as more filters migrate —
if the dependency tax keeps compounding, `rtk` growing a leaner `filters`-only feature flag
(no analytics/telemetry deps) is the right fix, not accepting unbounded binary growth per
migrated filter.

Commands on `RTK_WORTH_ROUTING` without a migrated in-process filter (`git`, `gh`, `npm`,
`docker`, etc.) still route through spawning the `rtk` binary — real, just not the fastest
tier yet. Migrate one function at a time into `rtk`'s `src/lib.rs` `filters` module as
production usage justifies it, same discipline that added `cargo_test` first (it was the
one actually seen in real production review scripts).

## Native functions available in a script

- `read_file(path) -> String` — errors clearly if the file doesn't exist
  or isn't valid UTF-8.
- `read_file(path, #{lines: "120-180"}) -> String` — only that slice.
  1-based and inclusive on both ends, like an editor and like
  `sed -n 'i,jp'`, not like a Rust slice. The cut happens on the Rust
  side and *stops* at the end line, so the rest of the file is never
  materialised. On a 1.3 MB file, taking 20 lines went from 1,348,894 B
  to 1,386 B per read.
- `read_files([path, ...]) -> Map` — reads the whole list and returns a
  map path → contents; takes the same `#{lines: ...}` option. Parallel
  above ~400 files, serial below it, because that is where the measured
  crossover is (see *Benchmark*). A map rather than an array so the
  caller never has to match by index.
- `write_file(path, content)` — creates parent directories as needed.
  **Refuses** to replace an existing file with content less than half its
  size: that shape is a wipe, not an update (see *Known traps* below).
- `write_file_force(path, content)` — same thing without the shrink
  guard, for when replacing a file with something much smaller is the
  actual intent.
- `append_file(path, content)` — appends, creating the file if needed.
  Use this instead of `read_file` + `write_file` whenever the goal is
  "add text to these files": there is no read step to get wrong, so
  nothing already in the file can be lost.
- `replaced(s, old, new) -> String` — non-mutating string replace. Rhai's
  own `s.replace(a, b)` mutates `s` in place and returns **unit**, so
  `let new = s.replace(a, b)` binds `()` and `() + text` collapses to
  `text`. Reach for `replaced` whenever you want a new string back; the
  runtime also warns, before running, when a script assigns the result of
  a mutating method.
- `edit_file(path, old, new)` — same safety semantics as the Claude Code
  `Edit` tool: fails if `old` isn't found, and fails if `old` matches
  more than once (ambiguous replace refused, not silently applied to the
  first match).
- `run_shell(cmd) -> String` — runs via `sh -c`, cwd locked to the
  sandbox workdir, stdout+stderr captured. Refuses commands matching the
  denylist below unless called as `run_shell(cmd, #{confirm: true})` or
  `run_shell_confirmed(cmd)`. **Auto-routed through [`rtk`](https://github.com/SouzaRodrigo61/rtk)
  when it's on PATH, `cmd` is a single plain command** (no `|`/`&&`/`;`/
  `>`/`<`/`` ` ``/`$(`), **and the first word is a known-heavy tool**
  (`cargo`, `npm`/`npx`/`pnpm`/`yarn`, `go`, `mvn`/`gradle`, `dotnet`,
  `make`, `pytest`/`jest`/`vitest`/`phpunit`/`rake`, `docker`, `kubectl`,
  `git`) — otherwise `run_shell` would bypass the same output trimming the
  top-level `Bash` tool already gets. Measured: `cargo test` through
  codemode+rtk on this repo returns `cargo test: 27 passed (4 suites,
  2.39s)` — 1 line instead of the ~50-line raw log, matching rtk's own
  ~99.6% reduction on that command.

  The allowlist exists because routing *everything* plain through `rtk`
  was tried first and made things worse, measured, not assumed: `rtk`'s
  own startup (~3.5ms, `rtk --help` alone) is already more than a small
  fast command costs end-to-end (`grep -c` on two tiny files: ~2ms raw) —
  routing those added latency for zero output-size win. There's also no
  separate `rtk`-availability probe before the routed call: that was a
  second full `rtk` process spawn stacked on top of the routed one,
  doubling exactly the cost this exists to avoid — the routed spawn is
  just attempted directly, falling back to `sh -c` only if the OS reports
  `rtk` isn't found (`io::ErrorKind::NotFound`). A command with shell
  syntax (pipe, redirect, chain) always goes through plain `sh -c`,
  unrouted — `rtk`'s subcommands take argv, not shell syntax, so splitting
  a pipeline into `rtk <first-word> <rest>` would silently drop everything
  after the first metacharacter.
- `run_shell_full(cmd) -> #{stdout, stderr, exit_code, success}` — the
  typed sibling of `run_shell` (issue #6, ported from DeepSeek Harness's
  typed-tool-returns): separate raw streams, integer exit code, boolean
  success, so a script can *branch* on results instead of scraping a
  merged prose string. Deliberately unfiltered — RTK compression exists
  for text headed back into the model's context, and it would destroy
  exactly the raw fields this contract promises. Rule of thumb: output
  you print/return → `run_shell` (filtered); output you branch on →
  `run_shell_full` (typed). Same denylist, same uncatchable refusal,
  same `#{confirm: true}` opt-in.
- `http_get(url) -> #{status, body, success}` — sandboxed HTTP GET gated
  by a **static host allowlist**: `codemode run --allow-host <h>`
  (repeatable; `h` allows any port, `h:p` exactly that port, no
  wildcards). Default-closed — no flag means every request is refused,
  and a disallowed host is the same uncatchable terminator as a denylist
  hit, so a script can't probe hosts in a try/catch loop. Only
  `http://`/`https://`; userinfo and bracketed IPv6 are refused rather
  than half-parsed (a mis-parsed host is an allowlist bypass). Fetching
  delegates to `curl` with a fixed argv — no shell, no redirect
  following (a 3xx comes back as the status itself, so a redirect can
  never hop to a host that was never allowed), 30s timeout, 10MB body
  cap. This exists so scripts stop reaching for `run_shell("curl ...")`,
  which routes around the network-boundary story entirely (issue #8; see
  the Check Point "agentic glue" research for why every native API is
  part of the boundary).
- `grep(pattern)` / `grep(pattern, path)` — shells out to `rg` if it's on
  `PATH`, otherwise falls back to a simple in-process substring search.
  Restricted to the sandbox workdir.
- `glob(pattern) -> Array` — via the `glob` crate, restricted to the
  sandbox workdir; every match is canonicalized and re-validated against
  the sandbox before being returned, so results are always paths
  `read_file`/`write_file`/`edit_file` accept.

## Pre-flight: the script is checked before anything runs

`codemode run` compiles the script, resolves **every function it calls**
against what is actually registered, and lints it — all before the first
primitive executes. A `Function not found: join` on line 8 used to surface
only after five `run_shell` calls had already run; now it costs ~5ms and
zero side effects.

```
codemode check script.rhai      # same pre-flight, never executes
codemode run script.rhai --dry-run   # announces every write/edit/shell, performs none
```

Three things fail the pre-flight:

- **an unknown function** — with the closest registered name suggested
- **a syntax error** — printed with the offending line and a caret under the column
- **assigning a mutating method** (`let t = s.trim()`) — these return unit in
  Rhai, and that exact shape wiped 70 files on 2026-08-19 without ever
  erroring. `replaced(s, old, new)` and `trimmed(s)` are the non-mutating forms.

Idioms from other languages (`=>`, `console.`, `format!`) print a hint
rather than failing: a false positive there would cost more than it saves.

## Long commands, loops, and more than one repo

A script may now do the thing the old 120s cap forbade: **edit, run the
suite, and decide by the result** — in one call.

```
codemode run verify.rhai --timeout 0        # no wall-clock limit on the script
codemode run verify.rhai --cmd-timeout 900  # but no single command may exceed 15min
codemode run x.rhai --extra-root ../other-repo   # a second confined root
```

Three independent guards, because they are three different failures:

| Flag | Guards against | Default |
|---|---|---|
| `--timeout` | the whole script running away; `0` disables | 30s |
| `--cmd-timeout` | one shell command hanging forever; `0` uses the plain blocking wait | 600s |
| `--vm-idle` | `loop {}` — time with **no primitive dispatched at all**, so it still fires under `--timeout 0` | 30s |

A command running longer than 10s prints a heartbeat to stderr. Watching a
command costs ~0.37ms per `run_shell` versus the old blocking wait; pass
`--cmd-timeout 0` to opt out.

`parallel_shell(["cmd a", "cmd b", ...])` runs commands concurrently, capped
at the machine's parallelism, and returns the `run_shell_full` maps **in
order**. The denylist is checked before anything is dispatched, so a refusal
can never hide inside a thread.

## Native search and glob respect your ignore rules

`grep(pattern, path)` uses the ripgrep walker (`ignore`), so it honors
`.gitignore`, skips `.git/`, sniffs binaries by their first 8 KB instead of
reading them whole, and walks in parallel. Results come back sorted by path,
because a parallel walk returns them out of order and a search result has to
be reproducible.

Measured on this repo (2.9 GB of `target/`): **3,062 ms → 13 ms**. The old
walker descended into everything and read every file as UTF-8 before finding
out it was a binary — it was four times slower than shelling out to `grep`,
which is exactly what the primitive existed to avoid.

`glob` follows the same rule, and it is a correctness fix as much as a speed
one: `glob("**/*.rs")` in this repo used to return **34 files in 17 ms**, 14
of them build artifacts nobody asked for. It now returns the **20 real
ones in 5 ms**, sorted. The escape hatch is naming the ignored path
yourself — `glob("target/**/*.rs")` still walks into `target/`, because
there the choice is explicit. Ignore rules govern where we wander on our
own, never what you asked for by name.

## Trivial commands never spawn a process

A census of 401 `run_shell` commands written by real scripts found that
**55% of them are trivial** — `cat`, `ls`, `grep`, `rm`, `cp`, `echo`,
`test`, `mkdir`, `touch`, `head`, `basename`, `dirname`, `true` — and each
one was paying 1.5–8.5 ms of process spawn. Those exact forms now run
in-process, at ~0.02 ms.

Measured: 50 trivial commands in a loop, **373 ms → 7.5 ms**.

The rule that makes this safe is *exact forms only*. An unrecognized flag,
a glob, a shell metachar, or a different arity falls through to the same
spawn as before — diverging from the real shell would be worse than being
slow, and every covered form has a test comparing its output byte-for-byte
against the real binary.

The pre-flight also names the primitive when a script shells out to
something that already exists natively (`cat` → `read_file`, `find` →
`glob`, `sed -i` → `replace_all_in_glob`), since a primitive returns typed
data instead of text to re-parse.

## Pipelines run without a shell in the middle

`cat a.txt | sort | head -n 5`, `cmd > file`, `cmd >> file`, `2>&1` — these
execute directly, and any stage that is a pure text filter (`head`, `tail`,
`wc -l`, `sort`, `sort -u`, `uniq`, `grep`, `grep -v`, `grep -c`, `cat`)
runs in memory with no process at all.

Measured: 15 pipelines plus 10 `sh -c` calls, **170 ms → 54 ms** (−68%).

Same rule as everywhere else: anything with a variable, a subshell, a glob,
`;`, `&&` or `||` goes to `sh`, which is what knows how to do that properly.
Every covered form has a test comparing against the real shell.

Commands whose output is already minimal or machine-readable
(`git rev-parse`, `git config`, anything with `--json`/`--jq`/`--porcelain`)
skip RTK routing: there is nothing for a text filter to compress, and the
extra process was costing ~15 ms. `git rev-parse HEAD` ten times: **281 ms →
129 ms**.

## Known traps

Paid for already — don't rediscover them:

- **Never emulate append with `read_file` + `write_file` over a batch.**
  `write_file` replaces the *whole* file, so any script bug that makes
  the assembled string shorter erases the rest of it — silently, across
  every file in the loop. This happened for real on 2026-08-19: 70
  markdown files reduced to just the block that was supposed to be
  appended. Use `append_file` to add, `edit_file` with an exact anchor to
  change part of a file, and run a batch on ONE file before running it on
  all of them. `write_file`'s shrink guard now refuses the wipe shape, but
  the guard is the backstop, not the plan.
- **Three independent guards, not one watchdog.** `--timeout` bounds the
  whole script (30s default, no hard cap — raise it); `--cmd-timeout`
  bounds a single shell command (600s default); `--vm-idle` catches a pure
  VM loop that never dispatches a primitive. Because `--cmd-timeout` is
  generous, **a test suite inside a script works** — what you raise is the
  global one: `codemode run ci.rhai --timeout 300`. An earlier version of
  this README said suites belonged outside the script; that was true when
  the single 30s watchdog was all there was.
- **Rhai is not JavaScript and not Rust.** No single-quoted strings and no
  `${}` interpolation outside backtick strings; functions are `fn`, not
  `function`; closures are `|x| expr`, not `x => expr`; no `let mut`, no
  `format!`, no `console.log`, no `require`/`import`. A failing script
  prints targeted hints for these.
- **`run_shell` aborts the script when the command fails** (non-zero
  exit), in every internal path — `sh`, in-process, RTK-routed. Before
  this was made uniform, the behaviour depended on which path handled the
  command: `git status` outside a repo threw while `cargo build` without a
  `Cargo.toml` was swallowed. Use **`run_shell_full(cmd)`** when failure is
  expected and the script must decide: it returns `#{stdout, stderr,
  exit_code, success}` and never aborts. Watch out for commands that use
  the exit code as a *boolean* (`test -f`, `grep -q`, `diff -q`) — those
  now abort; reach for the native primitive (`path_exists`) or
  `run_shell_full`.
- **Glob metacharacters in literal directory names** (`[id]`, `?`) are
  interpreted as patterns, so `glob("[id]/*.md")` matches nothing rather
  than the directory literally named `[id]`.

## Script library: `.codemode/` per repo

A bare script name that doesn't resolve as given is also looked up in
`<workdir>/.codemode/` — so a repo can keep a versioned library of
reusable scripts (`review.rhai`, `bump-and-verify.rhai`, ...) directly
runnable as `codemode run review.rhai`, instead of every session
re-deriving (and duplicating) the same script from scratch. That
duplication is the field-reported failure mode of code mode across
sessions (issue #9). Only bare names fall back; an explicit path that
doesn't exist fails loudly, never silently swapped for a library file.
Run `codemode list` before writing a new script — or before doing anything
by hand. That habit *is* the adoption gap: in one real session, `cargo
test`, `clippy`, `git commit/push` and `gh pr view` were run by hand dozens
of times while `ci.rhai` and `ship.rhai` sat versioned in the repo.

**Two different directories share the name `.codemode`:**

```
~/.codemode/        state. Created automatically on the first run.
                      runs.jsonl   telemetry, one line per run
                      last.rhai    the last script that came from stdin
                    Honours $CODEMODE_HOME.

<repo>/.codemode/   the library. Versioned in git. NEVER created
                    automatically — `codemode run` does not create it.
                    Only `codemode save <name>` does.
```

That surprises people: running a script inside a repo does **not** give
that repo a library. A repo without `.codemode/` isn't broken or
half-installed — nobody ran `save` there. Becoming a repo asset is a
deliberate act; if it were automatic, every throwaway script of every
session would become a versioned file.

```
codemode list                    # what this repo has, and how often each ran
codemode save verify --desc "…"  # promote the last script you ran
codemode run verify.rhai         # run it by bare name
codemode run review.rhai --arg 77   # ARGS[] makes one script serve many cases
```

`save` and `list` exist because the measured failure mode is agents rewriting
the same script every session. `--json` on a run returns `{output, exit_code,
prims, prim_total, calls_avoided, out_bytes, over_context, ms}` so the next
step can branch on data instead of scraping text, and
`replace_all_in_glob(pattern, old, new)` does the bulk edit that was being
hand-rolled as a loop of `edit_file`, returning the paths it touched.

Because the library is versioned, a one-shot migration for a closed issue
lives there forever, and every worktree carries a copy. `codemode list`
marks `obsoleto?` anything with no run in 30+ days — but only when *some*
script in that folder has history, because otherwise `0x` means "no data",
not "dead".

## When NOT to use it

One operation. A single read, a single edit, a single command: wrapping it in
Rhai costs more than the Bash call it replaces.

This is measured, and it is the most common mistake. In the real-usage
history, **a third of runs used exactly one primitive** — and those failed
**36%** of the time, against **5.6%** for scripts with three or more. The
one-liner probe is both the cheapest thing to get wrong and the most likely
to fail.

So a script collapsing fewer than two primitives warns on stderr, and
`--strict` refuses to run it at all. A refusal is recorded as a *refusal*,
not a failure — counting the guard as a failure would mean turning on the
defence against waste made the numbers worse. Scripts with a loop are exempt:
one call in the source can be N at runtime.

## Sandbox / security model

This is a prototype with real, tested guardrails — not a full container
sandbox. Same spirit of care as `rtk`/leanCTX elsewhere in this
ecosystem: confine what's cheap and reliable to confine, deny the
obviously destructive shell patterns by default, and be explicit in
this document about what's *not* covered rather than leaving it as a
silent gap.

**Filesystem confinement.** Every file operation (`read_file`,
`write_file`, `edit_file`, `glob`, and the cwd of `run_shell`) resolves
the path and checks the canonicalized result stays inside `--workdir`
(default: current directory). This is enforced three ways:
1. Absolute paths and `..` are resolved lexically and must land inside
   the workdir.
2. The longest *existing* ancestor of the target is canonicalized (which
   resolves symlinks) and re-checked against the workdir — this catches
   a symlink inside the workdir that points outside it.
3. `glob` results are re-validated individually after expansion.

Escape attempts fail loudly with a specific error; there is no silent
fallback.

**Command denylist (`src/denylist.rs`).** `run_shell` blocks by default:
`rm -rf`/`-fr` (and split `-r -f`/`--recursive --force`), `git push
--force`/`-f`, `git reset --hard`, `git clean -f`, `DROP TABLE`/`DROP
DATABASE`, `sudo`, reads of `.env`/`.ssh`/`id_rsa`/`credentials.json`,
and the classic `:(){ :|:& };:` fork bomb. A script can only run one of
these by explicitly opting in: `run_shell(cmd, #{confirm: true})`.

**No network primitives.** No HTTP/socket function is exposed to Rhai
scripts — this is intentional scope-limiting for the prototype, not a
hidden TODO. `run_shell` *can* still technically invoke `curl` or `git
push` because it's a real shell — the denylist above covers the most
common destructive network case (force-push), but this is **not** a
network sandbox. Documented, accepted risk for v1: if you don't trust a
script not to exfiltrate data via `run_shell`, don't run it.

**Timeout.** Default 30s, hard cap 120s regardless of what `--timeout`
requests. Two layers:
1. `Engine::on_progress` — Rhai calls this roughly once per VM
   operation; the callback checks elapsed time and aborts the script
   cleanly (`ErrorTerminated`) once the deadline passes. This is what
   kills a pure-Rhai infinite loop (`loop { }`).
2. A watchdog thread with `mpsc::recv_timeout`. `on_progress` cannot
   interrupt a script that is blocked *inside* a native call (e.g.
   `run_shell` running `sleep 999`) — Rhai isn't executing VM operations
   while waiting on a subprocess, so the progress hook never fires.
   **Rust has no safe way to forcibly kill a thread mid-execution**, so
   the watchdog's last resort is `std::process::exit(124)` for the whole
   process once the hard deadline passes, taking the stuck native call
   down with it. This is a known, deliberate limitation, not an
   oversight: it's a process kill, not a thread kill.

**Output cap + spill.** Default 1 MiB across everything printed by the
script (`print`/`debug` calls plus a non-unit return value). Past the
cap, stdout stays capped (head only) but nothing is lost: the FULL
stream, from byte zero, spills to a temp file
(`$TMPDIR/codemode-spill-<pid>.log` — `$TMPDIR` points at the session
scratchpad under the agent harnesses this runs in), and the stderr
notice names the file plus a tail preview of its last lines. Truncation
that silently discards the overflow reads as "covered everything" when
it didn't — the output-ledger lesson from DeepSeek Harness (issue #7):
overflow must be an explicit, recoverable condition, never a shorter
string disguised as the whole output. If the spill file can't be
created, the notice says so ("spill unavailable, overflow lost") instead
of pretending.

**Consolidated output.** Default stdout is exactly what the script
`print()`ed (plus its final expression value, if any) — not a log of
each primitive call. `--verbose` additionally prints workdir/timeout/cap
info to stderr for debugging; it does not change what a caller
downstream parses from stdout.

## Example script

`examples/bump_version.rhai` (paired with `examples/fixtures/*.conf`):

```rhai
let a = read_file("fixtures/a.conf");
let b = read_file("fixtures/b.conf");
let c = read_file("fixtures/c.conf");

let old_line = "";
for line in a.split("\n") {
    if line.starts_with("VERSION=") {
        old_line = line;
    }
}
if old_line == "" {
    throw "VERSION line not found in fixtures/a.conf";
}

let old_version = old_line.sub_string("VERSION=".len());
let parts = old_version.split(".");
let patch = parts[2].parse_int() + 1;
let new_version = parts[0] + "." + parts[1] + "." + patch;
let new_line = "VERSION=" + new_version;

edit_file("fixtures/b.conf", old_line, new_line);
edit_file("fixtures/c.conf", old_line, new_line);

let check = run_shell("grep -c 'VERSION=" + new_version + "' fixtures/b.conf fixtures/c.conf");

print("bumped VERSION " + old_version + " -> " + new_version + " in b.conf and c.conf");
print("verification:\n" + check);
```

Run it:

```bash
codemode run examples/bump_version.rhai --workdir examples
```

## What it actually saved, on real work

The honest number is the one from **real work**, with benchmark runs and
the tool's own development excluded. On the machine this was measured
(`~/.codemode/runs.jsonl`, August 2026):

| | before | after |
|---|---:|---:|
| real runs recorded | 9 | **36** |
| tool-calls avoided | 121 | **770** |
| failure rate | 30% | **13.9% lifetime · 5.0% rolling** |
| output per run | 4,549 B | **2,310 B** |

Output per run is the line that matters most: **bytes of context are what
cost tokens**, not tool-calls avoided. A script that collapses ten calls
and dumps 200 KB into the context is a net loss.

Two caveats stated up front, because a benchmark that flatters itself is
how this tool got into trouble in the first place:

- The jump from 121 to 770 avoided calls is partly accounting:
  `read_files` counts one primitive per file, which is the honest way to
  count what it replaced, but it is not all new work.
- Those numbers *are* the whole real-usage history on one machine. They
  are small. They are also the only ones not inflated by benchmark runs —
  see below for how badly that can go.

### The measurement that had to be fixed first

`codemode gain` used to sum everything. Of 1,312 recorded runs, **1,303
were the tool benchmarking and developing itself** and 9 were product
work. The report inflated the gain **145×** and hid a 30% real failure
rate behind the benchmark's 0.15%:

```
Execuções:                1308      <- benchmark + real, summed
Tool-calls evitadas:     57524      <- the real number was 121
Falhas:                      4  (0.3%)   <- it was 30%
```

Every run is now classified at write time (`kind`: real / bench / self /
check / refused), by rules that carry no machine-specific path list: a
script under `bench/`, a workdir in a temp root, or a workdir whose
`Cargo.toml` declares this crate. A legacy line whose workdir no longer
exists is reported as **unclassifiable**, never assumed to be real — of
36 such lines, 27 turned out to be deleted development worktrees.

### Per-feature, measured

| change | gain | condition |
|---|---|---|
| `read_file` with a line range | **973× fewer bytes**, 2.45× faster | reading part of a large file |
| `read_files` | 1.20× at 800 files, **1.40×** at 2,000 | only above ~400 files |
| library script vs. inline | **~60× cheaper per invocation** | ~10 tokens vs. 300–800 to author |

### And one honest negative

The agent-audit script in a companion repo was rewritten to use the new
primitives, to demonstrate the gain. It got **2× slower** — 16.7 ms
against 7.8 ms — because two directory-wide `grep` calls cost far more
than reading 44 small files. A single `read_files` of the whole files
merely tied. The script was reverted, with the measurement recorded in its
header so nobody repeats the attempt believing it is an improvement.

**The primitives above only pay past roughly 400 files.** Below that, the
straightforward path wins.

### The illustrative example

Task: read 3 config files, extract `VERSION` from one, apply the same
replace to the other two, run a verification command, report. That is
`examples/bump_version.rhai`, run against `examples/fixtures/{a,b,c}.conf`.

- **via codemode:** one `Bash` tool-call.
- **without it:** `Read(a)`, `Read(b)`, `Read(c)`, `Edit(b)`, `Edit(c)`,
  `Bash(grep)` — six tool-calls.

**6 → 1 for this task.** Treat it as an illustration of the shape, not as
a measured average — the table at the top of this section is the measured
part.

## The telemetry behind those numbers

Every run appends one JSON line to `~/.codemode/runs.jsonl` (override with
`CODEMODE_HOME`, disable with `CODEMODE_NO_TELEMETRY=1`). Metadata only —
a hash of the source, the per-primitive call counts the engine actually
dispatched, the *first word* of each shell command (`git`, `cargo`, `make` —
which is what answers "what deserves to become a native primitive?"), output
bytes, exit code, duration, workdir. Never the source itself, never file
contents, never command output, never command arguments, never `--arg`
values.
Writing the log is best-effort: if it fails, the run still succeeds.

```
codemode gain              # summary: calls avoided, error rate, waste buckets
codemode gain --history    # the last runs, one per line
codemode gain --json       # the aggregate, for scripting
```

```
codemode gain --bench      # the excluded segment, instead of real work
codemode gain --janela 0   # turn the rolling window off
```

Three things in that report earn their place:

- **Real work by default.** Benchmark runs, scratch scripts in a temp
  directory and the tool developing itself are reported separately. Runs
  refused by a guard (`--strict`) get their own `kind` and are *not*
  counted as failures — counting them meant that turning on the defence
  against waste made the failure rate worse.
- **Lifetime rate next to a rolling window.** A failure from three weeks
  ago weighs the same as today's, forever. With 5 historical failures in
  33 runs, the lifetime rate only drops below 5% after 68 consecutive
  clean runs — the target stops being a target and becomes a wait. The
  window answers the other question: *are we getting better?*
- **The buckets.** A script with 3+ primitives is a real collapse; one
  with a single primitive cost *more* than the equivalent `Bash` call.
  That bucket is measured waste, and it correlates with failure hard: in
  the real history, single-primitive scripts failed **36%** of the time
  against **5.6%** for scripts with three or more.

Without this log none of that was knowable; the first audit had to be
reverse-engineered out of the host CLI's transcripts.

## Tests

`cargo test` covers (see `src/sandbox.rs`, `src/denylist.rs`,
`tests/cli.rs`):
- path traversal (`../..`) blocked
- absolute-path escape blocked
- symlink pointing outside the workdir blocked
- `edit_file` fails when `old` is missing, and when it's ambiguous
  (multiple matches)
- `edit_file` succeeds and rewrites the file on a unique match
- `run_shell` refuses a denylisted command without `confirm: true`, and
  runs it when confirmed
- infinite loop (`loop { }`) is killed by the timeout (exit code `124`)
- output beyond `--max-output` is truncated with a stderr notice, and the full stream spills to a temp file named in that notice (with tail preview)
- output beyond `--max-context` warns without truncating, in both plain and `--json` mode, and the JSON carries `out_bytes` / `over_context` / `largest_print`
- `run_shell` aborts on a non-zero exit in all four internal paths, while `run_shell_full` stays tolerant
- `glob` handles absolute patterns, reaches `--extra-root`, and errors on a named directory that doesn't exist instead of returning an empty list
- `codemode idioms` still mentions every behaviour that changes how a script must be written
- stdin script input (`codemode run -`) works end to end

## Benchmark: codemode vs. native tool-calls

**`codemode bench`** times a script's real wall-clock cost natively — no Python, no shell
timing wrapper. That matters: an earlier Python-based harness for this same benchmark
measured ~0.8ms more overhead per subprocess spawn than Rust's own `Command::output()`
(2.44ms vs 1.64ms median spawning `/usr/bin/true`, 100 samples each) — and that tax
compounds faster on whichever side spawns more subprocesses per iteration, which is exactly
the variable being measured. `codemode bench` removes the confound: timer and process
spawning are both native, so the number is accurate on every CLI this binary ships to.

```bash
codemode bench examples/bump_version.rhai --workdir examples \
  --compare "cat fixtures/a.conf > /dev/null; cat fixtures/b.conf > /dev/null; cat fixtures/c.conf > /dev/null; sed -i '' 's/VERSION=1.0.0/VERSION=1.0.1/' fixtures/b.conf; sed -i '' 's/VERSION=1.0.0/VERSION=1.0.1/' fixtures/c.conf; grep -c 'VERSION=1.0.1' fixtures/b.conf fixtures/c.conf" \
  --reset-cmd "git checkout -- fixtures/"
```

| Script | Tokens | Tool-calls | Wall-clock (median, n=50, native timer) | vs. native |
|---|---:|---:|---:|---:|
| Native (3× Read, 2× Edit, 1× Bash) | 592 | 6 | 13.6ms | — |
| `bump_version.rhai` (verifies via `run_shell` + `grep`; `grep` isn't RTK-routed, see below) | 60 | 1 | 7.5ms | **1.81×** |
| `bump_version_optimized.rhai` (verifies in-process, zero subprocess) | ~55 | 1 | 2.8ms | **5.72×** |

These are post-fix numbers. An earlier version of `maestri::register()` ran an unconditional
`maestri --help` probe on every single `codemode run`, regardless of whether the script
called any `maestri_*` function — measured directly at ~4.3ms, more than the rest of a
typical invocation combined. Fixed by making availability lazy (checked by the actual
subprocess spawn inside each `maestri_*` call, not an upfront probe) — see `src/maestri.rs`.
Cut `bump_version_optimized.rhai` from 6.4ms to 2.8ms outright (2.08× → 5.72×). Worth
grepping this codebase for other unconditional `Command::new` calls before trusting a
"should be fast" assumption again — that's exactly how this one shipped unnoticed.

Re-run this yourself with `codemode bench` (see below) rather than trusting these numbers
verbatim — wall-clock varies run to run with whatever else is on the machine (this dev
box's own numbers moved between 1.25×/1.97× and 2.10×/5.66× across runs in the same
session, all real, none cherry-picked). Token counts and tool-call counts don't have that
noise; treat those as the stable half of this table.

**The real gap, found by profiling instead of assuming:** binary startup is ~2.8ms and
in-process file I/O is ~0.5ms — both already near the floor. The cost that actually matters
is subprocess spawning, ~4–8ms per spawn on this machine. `bump_version.rhai` verifies with
`run_shell("grep ...")` out of habit (a realistic thing for an agent to reach for, not a
strawman) — that one spawn is most of its 7.4ms. `bump_version_optimized.rhai` re-reads the
files it already wrote and checks with Rhai's own `.contains()` instead, spawning nothing —
**5.66× faster than native, not 2×,** just from writing the script to avoid an unnecessary
subprocess. The lesson generalizes: every `run_shell`/`grep`/`glob` call in a codemode
script is worth asking "does this need a real external tool, or can it be done with what's
already read into memory?"

The 15.4ms native number is still raw process/I/O time only — it excludes per-round-trip
LLM inference latency, which dominates real session cost and doesn't get fabricated here by
spending real API turns on a synthetic benchmark. The 6→1 round-trip reduction is the real
lever there. Artifact with the full breakdown:
https://claude.ai/code/artifact/971373b6-da99-4c2e-a7ff-31bd929f3e22 (numbers there predate
the native `bench` subcommand and the in-process-verify variant — this section supersedes
it; the artifact will be refreshed to match).
