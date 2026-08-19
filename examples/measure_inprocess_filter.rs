// Isolated measurement: pure in-process rtk::filters::cargo_test cost,
// comparable against `rtk pipe -f cargo-test` (measured separately at
// ~5.3ms, dominated by the rtk binary's own process spawn). Run with:
//   cargo run --release --example measure_inprocess_filter
use std::time::Instant;

fn main() {
    let raw = std::fs::read_to_string("/tmp/cargo_test_raw_output.txt")
        .expect("run cargo test > /tmp/cargo_test_raw_output.txt first");
    let n = 15;
    let mut times: Vec<f64> = Vec::new();
    for _ in 0..n {
        let t0 = Instant::now();
        let _ = rtk::filters::cargo_test(&raw);
        times.push(t0.elapsed().as_secs_f64());
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "in-process filters::cargo_test: median={:.4}ms min={:.4}ms max={:.4}ms",
        times[times.len() / 2] * 1000.0,
        times[0] * 1000.0,
        times[times.len() - 1] * 1000.0
    );
}
