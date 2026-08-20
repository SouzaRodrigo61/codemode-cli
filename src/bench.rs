//! `codemode bench` — native timing comparison, no external interpreter.
//!
//! Built after discovering that measuring codemode from a Python harness
//! adds real, non-trivial overhead of its own: `subprocess.run()` costs
//! ~0.8ms more per spawn than Rust's `Command::output()` for the exact same
//! child process (measured: 2.44ms vs 1.64ms median for spawning `/usr/bin/
//! true`, 100 samples each). That tax compounds faster on whichever side
//! spawns more subprocesses per iteration, which is exactly the variable
//! this benchmark exists to measure — so a Python-based harness biases its
//! own result. This subcommand removes the confound entirely: timer and
//! subprocess spawning are both native Rust (`std::time::Instant` /
//! `std::process::Command`), so every CLI this binary is installed on
//! (Claude Code, Codex, Grok Build, OpenCode, or a future one) gets the
//! same accurate, zero-interpreter-overhead measurement for free.

use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

pub struct BenchArgs {
    pub script: String,
    pub workdir: PathBuf,
    pub compare: Option<String>,
    pub reset_cmd: Option<String>,
    pub n: usize,
}

pub fn run(args: BenchArgs) -> Result<i32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("bench: can't find own executable: {e}"))?;

    let codemode_times = time_iterations(args.n, &args.reset_cmd, &args.workdir, || {
        let status = Command::new(&exe)
            .args(["run", &args.script, "--workdir"])
            .arg(&args.workdir)
            // Iteração de benchmark não é uso: sem isto, `bench --n 30` grava
            // 30 linhas em runs.jsonl e o `codemode gain` passa a reportar
            // trabalho que nunca existiu. Aconteceu de verdade: das 328
            // execuções registradas na série v1.0, 310 eram benchmark (#36).
            .env("CODEMODE_NO_TELEMETRY", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        status.map(|s| s.success()).unwrap_or(false)
    })?;

    report("codemode", &codemode_times);

    if let Some(compare_cmd) = &args.compare {
        let compare_times = time_iterations(args.n, &args.reset_cmd, &args.workdir, || {
            let status = Command::new("sh")
                .arg("-c")
                .arg(compare_cmd)
                .current_dir(&args.workdir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            status.map(|s| s.success()).unwrap_or(false)
        })?;
        report("compare", &compare_times);

        let cm_med = median(&codemode_times);
        let cmp_med = median(&compare_times);
        println!();
        if cm_med <= cmp_med {
            println!(
                "codemode is {:.2}x faster than the compare command ({:.3}ms saved per run)",
                cmp_med / cm_med,
                (cmp_med - cm_med) * 1000.0
            );
        } else {
            println!(
                "compare command is {:.2}x faster than codemode ({:.3}ms saved per run)",
                cm_med / cmp_med,
                (cm_med - cmp_med) * 1000.0
            );
        }
    }

    Ok(0)
}

fn time_iterations(
    n: usize,
    reset_cmd: &Option<String>,
    workdir: &PathBuf,
    mut op: impl FnMut() -> bool,
) -> Result<Vec<f64>, String> {
    let mut times = Vec::with_capacity(n);
    for i in 0..n {
        if let Some(cmd) = reset_cmd {
            let ok = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(workdir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return Err(format!("bench: reset command failed before iteration {i}"));
            }
        }
        let t0 = Instant::now();
        let ok = op();
        let elapsed = t0.elapsed().as_secs_f64();
        if !ok {
            return Err(format!("bench: measured command failed on iteration {i}"));
        }
        times.push(elapsed);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Ok(times)
}

fn median(times: &[f64]) -> f64 {
    times[times.len() / 2]
}

fn report(label: &str, times: &[f64]) {
    let med = median(times) * 1000.0;
    let min = times[0] * 1000.0;
    let max = times[times.len() - 1] * 1000.0;
    println!(
        "{label:12} median={med:7.3}ms  min={min:7.3}ms  max={max:7.3}ms  n={}",
        times.len()
    );
}
