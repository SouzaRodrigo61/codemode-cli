// Same task as bump_version.rhai, but the verification step avoids spawning
// a subprocess (grep/rg) entirely -- it just re-reads the files it already
// wrote and checks in-process with Rhai's own string ops. This is the
// idiomatic way to write a codemode script when the check doesn't need a
// real external tool: every subprocess spawn costs ~4-8ms on this machine
// (measured separately, see bench/), which dwarfs in-process file I/O
// (~0.1-0.2ms per read). bump_version.rhai (the run_shell version) is kept
// as the more "naturally written" baseline -- an agent reaching for grep
// out of habit is realistic, not a strawman.

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

// In-process verification: re-read and check with Rhai's own `contains`,
// no subprocess.
let b_after = read_file("fixtures/b.conf");
let c_after = read_file("fixtures/c.conf");
let matches = 0;
if b_after.contains(new_line) { matches += 1; }
if c_after.contains(new_line) { matches += 1; }

print("bumped VERSION " + old_version + " -> " + new_version + " in b.conf and c.conf");
print("verification: " + matches + "/2 files match (in-process, no subprocess spawned)");
