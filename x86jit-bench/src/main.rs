//! `x86jit-bench` — per-commit native vs interpreter vs JIT timing, stored as JSON
//! under `bench/history/<short-sha>.json` so results can be compared across
//! commits (evidence of what each change buys, and where).
//!
//! ```text
//! cargo run -p x86jit-bench --release -- record [--iters N]
//! cargo run -p x86jit-bench --release -- compare <refA> <refB>
//! cargo run -p x86jit-bench --release -- show <ref>
//! cargo run -p x86jit-bench --release -- list
//! ```
//!
//! Always run `--release` — debug timings are meaningless. Timings only compare on
//! the same host (each record tags its `host`/`cpu`); a dirty tree is flagged.

mod report;
mod workloads;

use std::time::{Duration, Instant};

use report::{Record, WlResult};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "record" => {
            let iters = flag_value(&args, "--iters").and_then(|v| v.parse().ok()).unwrap_or(3);
            record(iters);
        }
        "compare" if args.len() >= 3 => compare(&args[1], &args[2]),
        "show" if args.len() >= 2 => show(&args[1]),
        "list" => list(),
        "experiment" => experiment(),
        _ => {
            eprintln!(
                "usage:\n  record [--iters N]\n  compare <refA> <refB>\n  show <ref>\n  list"
            );
            std::process::exit(2);
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn median(mut xs: Vec<Duration>) -> Duration {
    xs.sort();
    xs[xs.len() / 2]
}

/// Time `f` `iters` times, returning the median and the first run's output.
fn time_it(iters: u32, mut f: impl FnMut() -> Vec<u8>) -> (Duration, Vec<u8>) {
    let mut out = Vec::new();
    let mut samples = Vec::with_capacity(iters as usize);
    for i in 0..iters {
        let t = Instant::now();
        let o = f();
        samples.push(t.elapsed());
        if i == 0 {
            out = o;
        }
    }
    (median(samples), out)
}

fn record(iters: u32) {
    let dirty = report::is_dirty();
    if dirty {
        eprintln!(
            "WARNING: working tree is dirty — these timings do NOT belong to {}.\n\
             Commit first for a clean record (recording anyway, dirty=true).",
            report::head_short()
        );
    }
    eprintln!(
        "recording {} \"{}\" on {} ({} iters)...",
        report::head_short(),
        report::head_subject(),
        report::hostname(),
        iters
    );

    let mut results = Vec::new();
    for wl in workloads::all() {
        // Interpreter.
        let (interp_t, interp_out) = time_it(iters, || (wl.guest)(workloads::interp()).0);
        // JIT (capture counters from a dedicated run so timing isn't perturbed).
        let (jit_t, jit_out) = time_it(iters, || (wl.guest)(workloads::jit()).0);
        let counters = (wl.guest)(workloads::jit()).1;
        // Native subprocess, if any.
        let native = wl.native.map(|nf| {
            let (t, out) = time_it(iters, nf);
            assert_eq!(out, wl.expect, "{}: native output != expected", wl.name);
            t
        });

        // Correctness gate — the bench also proves interp == JIT == expected.
        assert_eq!(interp_out, wl.expect, "{}: interpreter output != expected", wl.name);
        assert_eq!(jit_out, wl.expect, "{}: JIT output != expected", wl.name);

        let r = WlResult {
            name: wl.name.into(),
            kind: wl.kind.into(),
            native_ns: native.map(|d| d.as_nanos() as u64),
            interp_ns: interp_t.as_nanos() as u64,
            jit_ns: jit_t.as_nanos() as u64,
            chained: counters.chained,
            ibtc_filled: counters.ibtc_filled,
            fast_hits: counters.fast_hits,
            misses: counters.misses,
        };
        eprintln!("  {:<8} done", wl.name);
        results.push(r);
    }

    let rec = Record {
        commit: report::head_full(),
        commit_short: report::head_short(),
        subject: report::head_subject(),
        dirty,
        host: report::hostname(),
        cpu: report::cpu_model(),
        timestamp_unix: report::now_unix(),
        iters,
        workloads: results,
    };
    let path = report::save(&rec).expect("write record");
    println!("\nwrote {}", path.display());
    print_record(&rec);
}

fn ms(ns: u64) -> String {
    format!("{:.2}ms", ns as f64 / 1e6)
}

fn print_record(rec: &Record) {
    println!(
        "\n{} \"{}\"  host={} dirty={}",
        rec.commit_short, rec.subject, rec.host, rec.dirty
    );
    println!(
        "{:<8} {:<14} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "workload", "kind", "native", "interp", "jit", "jit/int", "jit/nat"
    );
    for w in &rec.workloads {
        let nat = w.native_ns.map(ms).unwrap_or_else(|| "-".into());
        let jn = w.jit_vs_native().map(|r| format!("{r:.1}x")).unwrap_or_else(|| "-".into());
        // 2 decimals so a sub-1 ratio (JIT slower than interp on one-shots) still
        // reads, instead of rounding to 0.0x.
        println!(
            "{:<8} {:<14} {:>10} {:>10} {:>10} {:>8.2}x {:>10}",
            w.name,
            w.kind,
            nat,
            ms(w.interp_ns),
            ms(w.jit_ns),
            w.jit_vs_interp(),
            jn
        );
    }
    println!("counters (JIT run):");
    for w in &rec.workloads {
        println!(
            "  {:<8} chained={} ibtc_filled={} fast_hits={} misses={}",
            w.name, w.chained, w.ibtc_filled, w.fast_hits, w.misses
        );
    }
}

fn show(reff: &str) {
    let short = report::resolve_short(reff);
    match report::load(&short) {
        Ok(rec) => print_record(&rec),
        Err(_) => {
            eprintln!("no record for {short} (bench/history/{short}.json). Run `record` there first.");
            std::process::exit(1);
        }
    }
}

fn load_or_die(reff: &str) -> Record {
    let short = report::resolve_short(reff);
    report::load(&short).unwrap_or_else(|_| {
        eprintln!("no record for {reff} -> {short}. Run `record` at that commit first.");
        std::process::exit(1);
    })
}

fn compare(ref_a: &str, ref_b: &str) {
    let a = load_or_die(ref_a);
    let b = load_or_die(ref_b);
    if a.host != b.host {
        eprintln!(
            "WARNING: different hosts ({} vs {}) — timings are not comparable.",
            a.host, b.host
        );
    }
    println!(
        "compare  A={} \"{}\"  ->  B={} \"{}\"  (host {})",
        a.commit_short, a.subject, b.commit_short, b.subject, b.host
    );
    println!(
        "{:<8} {:<7} {:>10} {:>10} {:>9}",
        "workload", "engine", "A", "B", "B vs A"
    );
    for wb in &b.workloads {
        let Some(wa) = a.workloads.iter().find(|w| w.name == wb.name) else {
            continue;
        };
        row("native", wa.native_ns, wb.native_ns, &wb.name);
        row("interp", Some(wa.interp_ns), Some(wb.interp_ns), &wb.name);
        row("jit", Some(wa.jit_ns), Some(wb.jit_ns), &wb.name);
    }
}

/// One compare row: A, B and the signed % change (negative = B faster).
fn row(engine: &str, a: Option<u64>, b: Option<u64>, name: &str) {
    let (Some(a), Some(b)) = (a, b) else {
        return;
    };
    let delta = (b as f64 - a as f64) / a as f64 * 100.0;
    let sign = if delta <= 0.0 { "" } else { "+" };
    println!(
        "{:<8} {:<7} {:>10} {:>10} {:>8}{:.1}%",
        name,
        engine,
        ms(a),
        ms(b),
        sign,
        delta
    );
}

/// One-off analysis (not stored): eager JIT vs hotness-gated tier-up at a few
/// thresholds, per workload. Shows how much one-shot compile cost tiering saves
/// and whether hot loops keep their win.
fn experiment() {
    let thresholds = [10u32, 50, 200];
    println!("hotness-gated tier-up: eager JIT vs tiered (median of 3)\n");
    println!(
        "{:<8} {:>10} {:>12} {:>12} {:>12}",
        "workload", "eager", "tier=10", "tier=50", "tier=200"
    );
    for wl in workloads::all() {
        std::env::remove_var("X86JIT_TIER");
        let (eager, out0) = time_it(3, || (wl.guest)(workloads::jit()).0);
        assert_eq!(out0, wl.expect, "{}: eager output != expected", wl.name);

        let mut cells = Vec::new();
        for thr in thresholds {
            std::env::set_var("X86JIT_TIER", thr.to_string());
            let (t, out) = time_it(3, || (wl.guest)(workloads::jit()).0);
            assert_eq!(out, wl.expect, "{}: tier={thr} output != expected", wl.name);
            let ratio = eager.as_secs_f64() / t.as_secs_f64();
            cells.push(format!("{} ({:.1}x)", ms(t.as_nanos() as u64), ratio));
        }
        std::env::remove_var("X86JIT_TIER");
        println!(
            "{:<8} {:>10} {:>12} {:>12} {:>12}",
            wl.name,
            ms(eager.as_nanos() as u64),
            cells[0],
            cells[1],
            cells[2]
        );
    }
    println!("\n(ratio = eager/tiered speedup; >1 means tiering is faster)");
}

fn list() {
    let recs = report::all_records();
    if recs.is_empty() {
        println!("no records yet. Run `record`.");
        return;
    }
    println!("{:<10} {:<10} subject", "commit", "host");
    for r in recs {
        let dirty = if r.dirty { " (dirty)" } else { "" };
        println!("{:<10} {:<10} {}{}", r.commit_short, r.host, r.subject, dirty);
    }
}
