// Representative "code mode" task, used for the tool-call measurement in
// the README: read 3 config files, extract a constant (VERSION) from the
// first one, bump it, apply the same replace to the other two files, run
// a verification command, and report the result — all in one process.

let a = read_file("fixtures/a.conf");
let b = read_file("fixtures/b.conf");
let c = read_file("fixtures/c.conf");

// Extract "VERSION=1.0.0" from a.conf.
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

// Apply the same replace to b.conf and c.conf (a.conf is left as the
// source of truth for what "old" was).
edit_file("fixtures/b.conf", old_line, new_line);
edit_file("fixtures/c.conf", old_line, new_line);

// Verification command.
let check = run_shell("grep -c 'VERSION=" + new_version + "' fixtures/b.conf fixtures/c.conf");

print("bumped VERSION " + old_version + " -> " + new_version + " in b.conf and c.conf");
print("verification:\n" + check);
