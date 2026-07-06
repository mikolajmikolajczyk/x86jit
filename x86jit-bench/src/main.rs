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
            let iters = flag_value(&args, "--iters")
                .and_then(|v| v.parse().ok())
                .unwrap_or(3);
            record(iters);
        }
        "compare" if args.len() >= 3 => compare(&args[1], &args[2]),
        "show" if args.len() >= 2 => show(&args[1]),
        "list" => list(),
        "gate" => {
            // Default higher than `record`: more samples give a cleaner minimum so
            // small/fast workloads (sha256) don't false-trip the threshold on noise.
            let iters = flag_value(&args, "--iters")
                .and_then(|v| v.parse().ok())
                .unwrap_or(7);
            gate(iters);
        }
        "experiment" => experiment(),
        _ => {
            eprintln!(
                "usage:\n  record [--iters N]   measure HEAD; write history + baseline + performance.md\n  \
                 gate [--iters N]     compare HEAD vs baseline; exit 1 on >threshold regression\n  \
                 compare <refA> <refB>\n  show <ref>\n  list"
            );
            std::process::exit(2);
        }
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Time `f` `iters` times, returning the **minimum** sample and the first run's
/// output. Min-of-N (the fastest run — the one least perturbed by OS scheduling,
/// interrupts, and frequency scaling) is the robust metric for regression
/// detection: noise only ever *adds* time, so the minimum best approximates the
/// code's intrinsic cost and min-vs-min is far more stable across runs than
/// median-vs-median (which the `gate` threshold relies on).
fn time_it(iters: u32, mut f: impl FnMut() -> Vec<u8>) -> (Duration, Vec<u8>) {
    let mut out = Vec::new();
    let mut best = Duration::MAX;
    for i in 0..iters {
        let t = Instant::now();
        let o = f();
        best = best.min(t.elapsed());
        if i == 0 {
            out = o;
        }
    }
    (best, out)
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

    // The baseline as it stands BEFORE this record, so `performance.md` shows the
    // delta this snapshot introduces.
    let prev_baseline = report::load_baseline();

    let rec = Record {
        commit: report::head_full(),
        commit_short: report::head_short(),
        subject: report::head_subject(),
        dirty,
        host: report::hostname(),
        cpu: report::cpu_model(),
        timestamp_unix: report::now_unix(),
        iters,
        workloads: run_workloads(iters),
    };
    let path = report::save(&rec).expect("write record");
    println!("\nwrote {}", path.display());
    // `record` also *accepts* this as the new baseline (the ratchet reference the
    // pre-push `gate` measures against) and refreshes the committed comparison doc.
    let bpath = report::save_baseline(&rec).expect("write baseline");
    let mpath = report::write_performance_md(&rec, prev_baseline.as_ref()).expect("write perf md");
    println!("wrote {}\nwrote {}", bpath.display(), mpath.display());
    print_record(&rec);
}

/// Run every workload three ways (interp/JIT/native), asserting interp == JIT ==
/// expected en route, and return the min-of-N timing results. Shared by `record`
/// (which stores them) and `gate` (which compares them to the baseline).
fn run_workloads(iters: u32) -> Vec<WlResult> {
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
        assert_eq!(
            interp_out, wl.expect,
            "{}: interpreter output != expected",
            wl.name
        );
        assert_eq!(jit_out, wl.expect, "{}: JIT output != expected", wl.name);

        eprintln!("  {:<8} done", wl.name);
        results.push(WlResult {
            name: wl.name.into(),
            kind: wl.kind.into(),
            native_ns: native.map(|d| d.as_nanos() as u64),
            interp_ns: interp_t.as_nanos() as u64,
            jit_ns: jit_t.as_nanos() as u64,
            chained: counters.chained,
            ibtc_filled: counters.ibtc_filled,
            fast_hits: counters.fast_hits,
            misses: counters.misses,
        });
    }
    results
}

/// Pre-push regression gate: measure HEAD and compare interp+JIT timings, per
/// workload, against the committed `bench/baseline.json`. Exits non-zero if any is
/// more than the threshold (default 10%, `X86JIT_PERF_THRESHOLD`) slower than the
/// baseline — unless `X86JIT_ALLOW_PERF_REGRESSION` is set. `record` moves the
/// baseline (accept an improvement, or a deliberate, allowed regression).
fn gate(iters: u32) {
    let threshold: f64 = std::env::var("X86JIT_PERF_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10.0);
    let allow = std::env::var("X86JIT_ALLOW_PERF_REGRESSION").is_ok();

    let Some(baseline) = report::load_baseline() else {
        eprintln!(
            "perf-gate: no baseline (bench/baseline.json) — run `cargo run -p x86jit-bench \
             --release -- record` to seed it. Skipping."
        );
        return;
    };
    if baseline.host != report::hostname() {
        eprintln!(
            "perf-gate: baseline host ({}) != this host ({}) — timings not comparable, skipping.",
            baseline.host,
            report::hostname()
        );
        return;
    }
    eprintln!(
        "perf-gate: measuring HEAD ({iters} iters) vs baseline {} \"{}\" (threshold {threshold:.0}%)...",
        baseline.commit_short, baseline.subject
    );
    let current = run_workloads(iters);

    // Gate on machine-speed-INVARIANT ratios, not absolute ns. A laptop's thermal /
    // frequency drift moves interp, JIT, and native together, so their ratios cancel
    // it (a run measured after a load spike is uniformly ~X% slower — absolute-ns
    // gating flags that as a regression; a ratio doesn't). Each ratio is "lower =
    // better"; a regression is one that grew past the threshold.
    println!(
        "{:<8} {:<14} {:>9} {:>9} {:>9}",
        "workload", "metric", "baseline", "current", "delta"
    );
    let mut regressions = Vec::new();
    for cw in &current {
        let Some(bw) = baseline.workloads.iter().find(|w| w.name == cw.name) else {
            continue;
        };
        // One-shot workloads run the guest once, so their JIT time is dominated by
        // compile cost over a tiny interpreter leg — an inherently noisy ratio, not a
        // codegen-quality signal. Measure + display them, but only *gate* the hot
        // workloads (dispatch-micro / compute-hot), where the JIT does real repeated
        // work and the ratio is stable run-to-run.
        let gated = cw.kind != "one-shot";
        for (label, br, cr) in ratio_pairs(bw, cw) {
            let delta = (cr / br - 1.0) * 100.0;
            let hit = gated && delta > threshold;
            println!(
                "{:<8} {:<14} {:>9.3} {:>9.3} {:>8}{:.1}%{}",
                cw.name,
                label,
                br,
                cr,
                if delta <= 0.0 { "" } else { "+" },
                delta,
                if hit {
                    "  <-- REGRESSION"
                } else if !gated {
                    "  (one-shot, not gated)"
                } else {
                    ""
                }
            );
            if hit {
                regressions.push(format!("{} {label} +{delta:.1}%", cw.name));
            }
        }
    }

    if regressions.is_empty() {
        eprintln!("perf-gate: OK — nothing over {threshold:.0}% slower than baseline.");
        return;
    }
    if allow {
        eprintln!(
            "perf-gate: {} regression(s), but X86JIT_ALLOW_PERF_REGRESSION is set — allowing. \
             Run `record` to accept them as the new baseline.",
            regressions.len()
        );
        return;
    }
    eprintln!(
        "\nperf-gate: BLOCKED — {} workload/engine over {threshold:.0}% slower than baseline:",
        regressions.len()
    );
    for r in &regressions {
        eprintln!("  {r}");
    }
    eprintln!(
        "If intended: re-run the push with X86JIT_ALLOW_PERF_REGRESSION=1, or `record` a new \
         baseline and commit it."
    );
    std::process::exit(1);
}

/// The machine-speed-invariant metric the `gate` compares: `jit_ns / interp_ns`
/// (lower = better — the JIT closer to / further ahead of the interpreter). Both
/// legs are measured back-to-back, in-process, in the same thermal state, so a
/// laptop's frequency/thermal drift cancels in the ratio. The interpreter is the
/// reference (not the native subprocess: native is a sub-millisecond fork/exec
/// dominated by startup + page-cache noise, so dividing by it *amplifies* variance).
/// This gates JIT codegen/dispatch regressions — the product's core; a pure
/// interpreter regression (both legs are separate code) is left to the diff tests.
fn ratio_pairs(b: &WlResult, c: &WlResult) -> Vec<(&'static str, f64, f64)> {
    vec![(
        "jit/interp",
        b.jit_ns as f64 / b.interp_ns as f64,
        c.jit_ns as f64 / c.interp_ns as f64,
    )]
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
        let jn = w
            .jit_vs_native()
            .map(|r| format!("{r:.1}x"))
            .unwrap_or_else(|| "-".into());
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
            eprintln!(
                "no record for {short} (bench/history/{short}.json). Run `record` there first."
            );
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
    const THR: u32 = 50;
    println!("tier-up modes: eager JIT vs inline tier vs background tier (min of 3)\n");
    println!(
        "{:<11} {:>10} {:>14} {:>14}",
        "workload",
        "eager",
        format!("inline={THR}"),
        format!("bg={THR}")
    );

    // The single-vcpu corpus (fib/sha/sqlite/lua): flip modes via env.
    for wl in workloads::all() {
        std::env::remove_var("X86JIT_TIER");
        std::env::remove_var("X86JIT_BG_TIER");
        let (eager, out0) = time_it(3, || (wl.guest)(workloads::jit()).0);
        assert_eq!(out0, wl.expect, "{}: eager output != expected", wl.name);

        std::env::set_var("X86JIT_TIER", THR.to_string());
        let (inline, out1) = time_it(3, || (wl.guest)(workloads::jit()).0);
        assert_eq!(out1, wl.expect, "{}: inline output != expected", wl.name);

        std::env::set_var("X86JIT_BG_TIER", "1");
        let (bg, out2) = time_it(3, || (wl.guest)(workloads::jit()).0);
        assert_eq!(out2, wl.expect, "{}: bg output != expected", wl.name);
        std::env::remove_var("X86JIT_TIER");
        std::env::remove_var("X86JIT_BG_TIER");

        println!(
            "{:<11} {:>10} {:>14} {:>14}",
            wl.name,
            ms(eager.as_nanos() as u64),
            speed(eager, inline),
            speed(eager, bg),
        );
    }

    // go-startup: over the threaded driver + Reserved span (its own runner).
    let (go_eager, oe) = time_it(3, || workloads::go_startup(workloads::jit(), None, false));
    assert_eq!(oe, workloads::GO_HELLO_OUT, "go eager output != expected");
    let (go_inline, oi) = time_it(3, || {
        workloads::go_startup(workloads::jit(), Some(THR), false)
    });
    assert_eq!(oi, workloads::GO_HELLO_OUT, "go inline output != expected");
    let (go_bg, ob) = time_it(3, || {
        workloads::go_startup(workloads::jit(), Some(THR), true)
    });
    assert_eq!(ob, workloads::GO_HELLO_OUT, "go bg output != expected");
    println!(
        "{:<11} {:>10} {:>14} {:>14}",
        "go-startup",
        ms(go_eager.as_nanos() as u64),
        speed(go_eager, go_inline),
        speed(go_eager, go_bg),
    );

    println!("\n(cell = time (speedup vs eager); >1x means faster than eager JIT)");
}

/// Format `t` as `ms (Nx)` where the ratio is the speedup vs `base`.
fn speed(base: Duration, t: Duration) -> String {
    format!(
        "{} ({:.1}x)",
        ms(t.as_nanos() as u64),
        base.as_secs_f64() / t.as_secs_f64()
    )
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
        println!(
            "{:<10} {:<10} {}{}",
            r.commit_short, r.host, r.subject, dirty
        );
    }
}
