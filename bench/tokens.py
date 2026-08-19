import json
import tiktoken

enc = tiktoken.get_encoding("cl100k_base")


def toks(s):
    return len(enc.encode(s))


def numbered(path):
    """Mimic Claude Code's Read tool output format: cat -n style."""
    lines = open(path).read().splitlines()
    return "\n".join(f"{i+1:>6}\t{line}" for i, line in enumerate(lines))


FIXDIR = "../examples/fixtures"

# ---- native path: 3x Read, 2x Edit, 1x Bash --------------------------------

native_pairs = []

for f in ["a.conf", "b.conf", "c.conf"]:
    tool_use = json.dumps({"type": "tool_use", "name": "Read", "input": {"file_path": f"{FIXDIR}/{f}"}})
    tool_result = numbered(f"{FIXDIR}/{f}")
    native_pairs.append(("Read " + f, tool_use, tool_result))

for f, old, new in [("b.conf", "VERSION=1.0.0", "VERSION=1.0.1"), ("c.conf", "VERSION=1.0.0", "VERSION=1.0.1")]:
    tool_use = json.dumps({"type": "tool_use", "name": "Edit", "input": {"file_path": f"{FIXDIR}/{f}", "old_string": old, "new_string": new}})
    tool_result = f"The file {FIXDIR}/{f} has been updated. Here's the result of running `cat -n` on a snippet of the edited file:\n     1\tservice = worker\n     2\t{new}\n     3\tretries = 3"
    native_pairs.append(("Edit " + f, tool_use, tool_result))

tool_use = json.dumps({"type": "tool_use", "name": "Bash", "input": {"command": f"grep -c 'VERSION=1.0.1' {FIXDIR}/b.conf {FIXDIR}/c.conf"}})
tool_result = f"{FIXDIR}/b.conf:1\n{FIXDIR}/c.conf:1"
native_pairs.append(("Bash verify", tool_use, tool_result))

native_total = 0
print("=== NATIVE (6 tool-calls) ===")
for label, tu, tr in native_pairs:
    t = toks(tu) + toks(tr)
    native_total += t
    print(f"  {label:14s} tool_use={toks(tu):4d}  tool_result={toks(tr):4d}  subtotal={t:4d}")
print(f"  TOTAL: {native_total} tokens across {len(native_pairs)} tool-calls\n")

# ---- codemode path: 1x Bash --------------------------------------------------

cm_tool_use = json.dumps({"type": "tool_use", "name": "Bash", "input": {"command": "codemode run bump_version.rhai --workdir examples"}})
cm_tool_result = 'bumped VERSION 1.0.0 -> 1.0.1 in b.conf and c.conf\nverification:\n2\n'

cm_total = toks(cm_tool_use) + toks(cm_tool_result)
print("=== CODEMODE (1 tool-call) ===")
print(f"  Bash codemode  tool_use={toks(cm_tool_use):4d}  tool_result={toks(cm_tool_result):4d}  subtotal={cm_total:4d}\n")

print("=== SUMMARY ===")
print(f"  native:   {native_total} tokens, 6 tool-calls")
print(f"  codemode: {cm_total} tokens, 1 tool-call")
print(f"  reduction: {(1 - cm_total/native_total)*100:.1f}% fewer tokens, {(1 - 1/6)*100:.1f}% fewer tool-calls")
