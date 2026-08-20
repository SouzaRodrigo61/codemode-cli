//! `codemode gain` — o relatório que torna o valor do codemode defensável
//! com número em vez de intuição (issue #12), espelhando `rtk gain`.
//!
//! Lê o que `telemetry` gravou; não mede nada por conta própria.

use crate::telemetry::{self, Entry};
use std::collections::BTreeMap;

pub struct GainArgs {
    pub history: bool,
    pub json: bool,
    pub limit: usize,
}

struct Agg {
    runs: usize,
    falhas: usize,
    prim_total: u64,
    calls_avoided: u64,
    out_bytes: u64,
    ms: u64,
    /// Buckets por número de primitivas: o desperdício medido mora aqui.
    b0: usize,
    b1: usize,
    b2: usize,
    b3: usize,
    de_biblioteca: usize,
    prims: BTreeMap<String, u64>,
    por_script: BTreeMap<String, usize>,
}

fn aggregate(entries: &[Entry]) -> Agg {
    let mut a = Agg {
        runs: entries.len(),
        falhas: 0,
        prim_total: 0,
        calls_avoided: 0,
        out_bytes: 0,
        ms: 0,
        b0: 0,
        b1: 0,
        b2: 0,
        b3: 0,
        de_biblioteca: 0,
        prims: BTreeMap::new(),
        por_script: BTreeMap::new(),
    };
    for e in entries {
        if !e.ok() {
            a.falhas += 1;
        }
        a.prim_total += e.prim_total;
        a.calls_avoided += e.calls_avoided();
        a.out_bytes += e.out_bytes;
        a.ms += e.ms;
        match e.prim_total {
            0 => a.b0 += 1,
            1 => a.b1 += 1,
            2 => a.b2 += 1,
            _ => a.b3 += 1,
        }
        if e.source == "lib" {
            a.de_biblioteca += 1;
        }
        for (k, v) in &e.prims {
            *a.prims.entry(k.clone()).or_insert(0) += v;
        }
        let rotulo = e.name.clone().unwrap_or_else(|| format!("<{}> {}", e.source, e.script));
        *a.por_script.entry(rotulo).or_insert(0) += 1;
    }
    a
}

fn pct(parte: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        parte as f64 * 100.0 / total as f64
    }
}

pub fn run(args: GainArgs) -> Result<i32, String> {
    let entries = telemetry::load();
    if entries.is_empty() {
        let onde = telemetry::log_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<HOME indefinido>".into());
        println!("codemode gain: nenhuma execução registrada ainda ({onde})");
        return Ok(0);
    }
    let a = aggregate(&entries);

    if args.json {
        println!("{}", json_report(&a));
        return Ok(0);
    }

    if args.history {
        println!("Últimas {} execuções", args.limit.min(entries.len()));
        println!("{:-<78}", "");
        for e in entries.iter().rev().take(args.limit) {
            println!(
                "{:>10}  {:<28} {:>3} prim  {:>6}ms  {:>7}B  {}",
                e.ts,
                e.name.clone().unwrap_or_else(|| format!("<{}>", e.source)),
                e.prim_total,
                e.ms,
                e.out_bytes,
                if e.ok() { "ok" } else { "ERRO" }
            );
        }
        println!();
    }

    println!("codemode gain");
    println!("{:=<78}", "");
    println!("Execuções:            {:>8}", a.runs);
    println!("Primitivas:           {:>8}", a.prim_total);
    println!("Tool-calls evitadas:  {:>8}", a.calls_avoided);
    println!("Falhas:               {:>8}  ({:.1}%)", a.falhas, pct(a.falhas, a.runs));
    println!("Saída total:          {:>8}B", a.out_bytes);
    println!("Tempo total:          {:>8}ms", a.ms);
    println!();
    println!("Por bucket de primitivas");
    println!("{:-<78}", "");
    linha_bucket("3+  colapso real", a.b3, a.runs);
    linha_bucket("2   marginal", a.b2, a.runs);
    linha_bucket("1   desperdício (use Bash direto)", a.b1, a.runs);
    linha_bucket("0   nenhuma primitiva", a.b0, a.runs);
    println!();
    linha_bucket("origem: biblioteca .codemode/", a.de_biblioteca, a.runs);
    println!();

    println!("Primitivas mais usadas");
    println!("{:-<78}", "");
    let mut prims: Vec<_> = a.prims.iter().collect();
    prims.sort_by(|x, y| y.1.cmp(x.1));
    for (nome, n) in prims.iter().take(10) {
        println!("  {nome:<24} {n:>6}");
    }
    println!();

    println!("Scripts mais reexecutados");
    println!("{:-<78}", "");
    let mut scripts: Vec<_> = a.por_script.iter().collect();
    scripts.sort_by(|x, y| y.1.cmp(x.1));
    for (nome, n) in scripts.iter().take(10) {
        println!("  {nome:<48} {n:>4}x");
    }
    Ok(0)
}

fn linha_bucket(rotulo: &str, n: usize, total: usize) {
    let p = pct(n, total);
    let barras = ((p / 5.0).round() as usize).min(20);
    println!("  {:<34} {:>5}  {:>5.1}%  {}", rotulo, n, p, "█".repeat(barras));
}

fn json_report(a: &Agg) -> String {
    let prims: Vec<String> =
        a.prims.iter().map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap_or_default(), v)).collect();
    format!(
        "{{\"runs\":{},\"falhas\":{},\"prim_total\":{},\"calls_avoided\":{},\"out_bytes\":{},\"ms\":{},\
\"buckets\":{{\"0\":{},\"1\":{},\"2\":{},\"3+\":{}}},\"de_biblioteca\":{},\"prims\":{{{}}}}}",
        a.runs,
        a.falhas,
        a.prim_total,
        a.calls_avoided,
        a.out_bytes,
        a.ms,
        a.b0,
        a.b1,
        a.b2,
        a.b3,
        a.de_biblioteca,
        prims.join(",")
    )
}
