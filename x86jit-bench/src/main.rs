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

use report::{Record, Stat, WlResult};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "record" => {
            let iters = flag_value(&args, "--iters")
                .and_then(|v| v.parse().ok())
                .unwrap_or(15);
            let warmup = flag_value(&args, "--warmup")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2);
            record(iters, warmup);
        }
        "compare" if args.len() >= 3 => compare(&args[1], &args[2]),
        "show" if args.len() >= 2 => show(&args[1]),
        "list" => list(),
        "trend" => trend(),
        "gate" => {
            // The noise-aware gate (PB-1) compares medians against a MAD noise band, so
            // a moderate sample count with warmup suffices — no need for `record`'s
            // heavier run (the one-shot workloads are ~1 s each, so more iters is slow
            // on the pre-push path).
            let iters = flag_value(&args, "--iters")
                .and_then(|v| v.parse().ok())
                .unwrap_or(9);
            let warmup = flag_value(&args, "--warmup")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2);
            gate(iters, warmup);
        }
        "experiment" => experiment(),
        "dump" => dump(),
        _ => {
            eprintln!(
                "usage:\n  record [--iters N] [--warmup W]   measure HEAD; write history + baseline + performance.md\n  \
                 gate [--iters N] [--warmup W]     compare HEAD vs the rolling-median reference; exit 1 on a regression past max(threshold, noise band)\n  \
                 trend [N]            last N records' jit/interp ratio per workload\n  \
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
/// output. Min-of-N (least perturbed by OS scheduling / interrupts / frequency
/// scaling — noise only ever adds time). Used by the ad-hoc `experiment` subcommand;
/// `record`/`gate` use [`time_stat`] for the full distribution.
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

/// Time `f` over `warmup + iters` runs, discarding the first `warmup` (cold I-cache,
/// page faults, frequency ramp), and return the kept samples' [`Stat`] (min / median
/// / MAD / n) plus the first run's output (perf-bench v2, doc-29 PB-1). The median +
/// MAD are what the noise-aware gate needs; `min` is kept as the intrinsic-cost
/// estimate.
fn time_stat(iters: u32, warmup: u32, mut f: impl FnMut() -> Vec<u8>) -> (Stat, Vec<u8>) {
    let mut out = Vec::new();
    let mut samples: Vec<u64> = Vec::with_capacity(iters as usize);
    let total = warmup + iters.max(1);
    for i in 0..total {
        let t = Instant::now();
        let o = f();
        let ns = t.elapsed().as_nanos() as u64;
        if i == 0 {
            out = o;
        }
        if i >= warmup {
            samples.push(ns);
        }
    }
    (stat_of(&mut samples), out)
}

/// min / median / MAD (median absolute deviation) over `samples` (sorted in place).
fn stat_of(samples: &mut [u64]) -> Stat {
    samples.sort_unstable();
    let n = samples.len();
    let median_ns = samples[n / 2];
    let mut dev: Vec<u64> = samples.iter().map(|&x| x.abs_diff(median_ns)).collect();
    dev.sort_unstable();
    Stat {
        min_ns: samples[0],
        median_ns,
        mad_ns: dev[dev.len() / 2],
        n: n as u32,
    }
}

fn record(iters: u32, warmup: u32) {
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

    let workloads = run_workloads(iters, warmup, true);
    // Machine-quality tag (PB-1): a record taken under host load is noisy and is not
    // eligible as a rolling-median reference (PB-4).
    let loadavg1 = report::loadavg1();
    let quality = report::quality(dirty, loadavg1);
    if quality == "loaded" {
        eprintln!(
            "WARNING: loadavg {:.1} is high for {} cores — this record is tagged `loaded` (noisy).",
            loadavg1.unwrap_or(0.0),
            report::num_cpus()
        );
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
        workloads,
        loadavg1,
        quality: Some(quality),
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
fn run_workloads(iters: u32, warmup: u32, modes: bool) -> Vec<WlResult> {
    use workloads::TierCfg;
    let mut results = Vec::new();
    for wl in workloads::all() {
        // Interpreter.
        let (interp, interp_out) = time_stat(iters, warmup, || {
            (wl.guest)(workloads::interp(), TierCfg::EAGER).0
        });
        // JIT eager (capture counters from a dedicated run so timing isn't perturbed).
        let (jit, jit_out) = time_stat(iters, warmup, || {
            (wl.guest)(workloads::jit(), TierCfg::EAGER).0
        });
        let counters = (wl.guest)(workloads::jit(), TierCfg::EAGER).1;
        // Native subprocess, if any.
        let native = wl.native.map(|nf| {
            let (s, out) = time_stat(iters, warmup, nf);
            assert_eq!(out, wl.expect, "{}: native output != expected", wl.name);
            s
        });
        // Deployment tiering modes (tiering track) — only in `record` (`modes`), not
        // the pre-push `gate` (which stays fast). `tier(50)` mirrors what `x86jit-run`
        // ships; `bg(50)` overlaps compile with interpretation.
        let (tier, bg, region_bg) = if modes {
            let (t, _) = time_stat(iters, warmup, || {
                (wl.guest)(workloads::jit(), TierCfg::tier(TIER_N)).0
            });
            let (b, _) = time_stat(iters, warmup, || {
                (wl.guest)(workloads::jit(), TierCfg::bg(TIER_N)).0
            });
            // BGT-6 region-bg: a region-forming backend with background tier-up.
            let (r, _) = time_stat(iters, warmup, || {
                (wl.guest)(workloads::jit_regions(), TierCfg::bg(TIER_N)).0
            });
            (Some(t), Some(b), Some(r))
        } else {
            (None, None, None)
        };

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
            // The `*_ns` fields stay as the min (pre-v2 shape, back-compat); the full
            // distributions ride in `*_stat` (perf-bench v2, PB-1).
            native_ns: native.map(|s| s.min_ns),
            interp_ns: interp.min_ns,
            jit_ns: jit.min_ns,
            chained: counters.chained,
            ibtc_filled: counters.ibtc_filled,
            fast_hits: counters.fast_hits,
            misses: counters.misses,
            interp_stat: Some(interp),
            jit_stat: Some(jit),
            native_stat: native,
            compile_ns: Some(counters.compile_ns),
            tier_stat: tier,
            bg_stat: bg,
            region_bg_stat: region_bg,
        });
    }
    results
}

/// The tier-up threshold the bench measures the tiered modes at — matches
/// `x86jit-run`'s shipped default (a block interprets 50× before it JIT-compiles).
const TIER_N: u32 = 50;

/// Pre-push regression gate: measure HEAD and compare interp+JIT timings, per
/// workload, against the committed `bench/baseline.json`. Exits non-zero if any is
/// more than the threshold (default 10%, `X86JIT_PERF_THRESHOLD`) slower than the
/// baseline — unless `X86JIT_ALLOW_PERF_REGRESSION` is set. `record` moves the
/// baseline (accept an improvement, or a deliberate, allowed regression).
fn gate(iters: u32, warmup: u32) {
    let threshold: f64 = std::env::var("X86JIT_PERF_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10.0);
    // Noise-band multiplier (perf-bench v2 PB-1, M5): a delta counts as a regression
    // only if it exceeds `max(threshold, NOISE_C · propagated MAD/median)`, so a
    // metric whose own jitter is ±X% needs a > ±X-ish% shift to trip. Kills the
    // task-146 false-positive class. (Between-*invocation* thermal drift is finished
    // off by PB-4's rolling-median reference.)
    let noise_c: f64 = std::env::var("X86JIT_PERF_NOISE_C")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3.0);
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
    // A gate run under host load is unreliable: the jit/interp ratio isn't perfectly
    // machine-state-invariant (the two legs respond differently to contention/thermal),
    // so a loaded run reads systematically off. Rather than false-block (the task-146
    // failure mode), measure + display but do NOT block when loaded — unless
    // X86JIT_PERF_FORCE is set. (`record` already tags such records `loaded` so they
    // never enter the PB-4 reference window.)
    let loadavg1 = report::loadavg1();
    let loaded = report::quality(false, loadavg1) == "loaded";
    let force = std::env::var("X86JIT_PERF_FORCE").is_ok();
    if loaded && !force {
        eprintln!(
            "perf-gate: loadavg {:.1} is high for {} cores — measuring for info but NOT blocking \
             (set X86JIT_PERF_FORCE=1 to gate anyway).",
            loadavg1.unwrap_or(0.0),
            report::num_cpus()
        );
    }
    let gate_active = !loaded || force;
    eprintln!(
        "perf-gate: measuring HEAD ({iters} iters, {warmup} warmup) vs baseline {} \"{}\" (threshold {threshold:.0}%, noise ×{noise_c:.0})...",
        baseline.commit_short, baseline.subject
    );
    let current = run_workloads(iters, warmup, false);

    // Reference = the rolling window of recent clean records (PB-4), not one baseline
    // point. For each workload the reference ratio is the window's MEDIAN jit/interp
    // ratio and the noise band is that window's MAD — the *between-invocation* spread
    // (thermal/frequency drift across separate runs), which PB-1's within-run MAD
    // could not see. A regression must clear `max(threshold, band)`. With < 2 clean
    // records the window can't estimate a spread, so it falls back to the single
    // baseline + PB-1's within-run propagated band.
    let win_k: usize = std::env::var("X86JIT_PERF_WINDOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let window = report::clean_recent(&report::hostname(), win_k);
    println!(
        "{:<8} {:<10} {:>9} {:>9} {:>8} {:>8} {:>6}",
        "workload", "metric", "ref", "cur", "delta", "band", "src"
    );
    let mut regressions = Vec::new();
    for cw in &current {
        // One-shot workloads are compile-dominated (JIT time ≈ compilation over a tiny
        // leg) — a noisy ratio, not a codegen-quality signal. Measure + show them, but
        // only *gate* the hot workloads where the JIT does real repeated work.
        let gated = cw.kind != "one-shot";
        let (ci, cj) = (cw.interp(), cw.jit_cold());
        let cr = cj.median_ns as f64 / ci.median_ns as f64;

        // Window ratios for this workload (each clean record's own jit/interp median).
        let refs: Vec<f64> = window
            .iter()
            .filter_map(|r| r.workloads.iter().find(|w| w.name == cw.name))
            .map(|w| w.jit_cold().median_ns as f64 / w.interp().median_ns as f64)
            .collect();

        let (br, band, src) = if refs.len() >= 2 {
            let m = report::median(&refs).unwrap();
            let b = if m > 0.0 {
                noise_c * report::mad(&refs, m) / m * 100.0
            } else {
                0.0
            };
            (m, b, format!("win{}", refs.len()))
        } else if let Some(bw) = baseline.workloads.iter().find(|w| w.name == cw.name) {
            let (bi, bj) = (bw.interp(), bw.jit_cold());
            let q = ci.rel_noise().powi(2)
                + cj.rel_noise().powi(2)
                + bi.rel_noise().powi(2)
                + bj.rel_noise().powi(2);
            (
                bj.median_ns as f64 / bi.median_ns as f64,
                noise_c * q.sqrt() * 100.0,
                "base".to_string(),
            )
        } else {
            continue;
        };

        let delta = (cr / br - 1.0) * 100.0;
        let limit = threshold.max(band);
        let hit = gate_active && gated && delta > limit;
        println!(
            "{:<8} {:<10} {:>9.3} {:>9.3} {:>7}{:.1}% {:>7.1}% {:>6}{}",
            cw.name,
            "jit/int",
            br,
            cr,
            if delta <= 0.0 { "" } else { "+" },
            delta,
            band,
            src,
            if hit {
                "  <-- REGRESSION"
            } else if !gated {
                "  (one-shot, not gated)"
            } else {
                ""
            }
        );
        if hit {
            regressions.push(format!(
                "{} jit/int +{delta:.1}% (ref {src}, band {band:.1}%)",
                cw.name
            ));
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

fn ms(ns: u64) -> String {
    format!("{:.2}ms", ns as f64 / 1e6)
}

fn print_record(rec: &Record) {
    println!(
        "\n{} \"{}\"  host={} dirty={}",
        rec.commit_short, rec.subject, rec.host, rec.dirty
    );
    println!(
        "{:<8} {:<14} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8} {:>8}",
        "workload", "kind", "native", "interp", "jit-cold", "compile", "run", "jit/int", "jit/nat"
    );
    for w in &rec.workloads {
        let nat = w.native_ns.map(ms).unwrap_or_else(|| "-".into());
        let jn = w
            .jit_vs_native()
            .map(|r| format!("{r:.1}x"))
            .unwrap_or_else(|| "-".into());
        // compile / run split (PB-2): `run` = steady-state execute (cold − compile).
        let (compile, run) = match w.run() {
            Some(r) => (ms(w.compile()), ms(r.min_ns)),
            None => ("-".into(), "-".into()),
        };
        // 2 decimals so a sub-1 ratio (JIT slower than interp on one-shots) still
        // reads, instead of rounding to 0.0x.
        println!(
            "{:<8} {:<14} {:>10} {:>10} {:>10} {:>10} {:>10} {:>7.2}x {:>8}",
            w.name,
            w.kind,
            nat,
            ms(w.interp_ns),
            ms(w.jit_ns),
            compile,
            run,
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
    println!(
        "tier-up modes: eager JIT vs inline tier vs background tier vs region-bg \
         (BGT-6, min of 3)\n"
    );
    println!(
        "{:<11} {:>10} {:>14} {:>14} {:>14}",
        "workload",
        "eager",
        format!("inline={THR}"),
        format!("bg={THR}"),
        format!("region-bg={THR}"),
    );

    // The single-vcpu corpus (fib/sha/sqlite/lua) across the JIT modes.
    use workloads::TierCfg;
    for wl in workloads::all() {
        let (eager, out0) = time_it(3, || (wl.guest)(workloads::jit(), TierCfg::EAGER).0);
        assert_eq!(out0, wl.expect, "{}: eager output != expected", wl.name);

        let (inline, out1) = time_it(3, || (wl.guest)(workloads::jit(), TierCfg::tier(THR)).0);
        assert_eq!(out1, wl.expect, "{}: inline output != expected", wl.name);

        let (bg, out2) = time_it(3, || (wl.guest)(workloads::jit(), TierCfg::bg(THR)).0);
        assert_eq!(out2, wl.expect, "{}: bg output != expected", wl.name);

        // BGT-6: region-forming backend + bg — hot loops tier up to background regions.
        let (rbg, out3) = time_it(3, || {
            (wl.guest)(workloads::jit_regions(), TierCfg::bg(THR)).0
        });
        assert_eq!(out3, wl.expect, "{}: region-bg output != expected", wl.name);

        println!(
            "{:<11} {:>10} {:>14} {:>14} {:>14}",
            wl.name,
            ms(eager.as_nanos() as u64),
            speed(eager, inline),
            speed(eager, bg),
            speed(eager, rbg),
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
    let (go_rbg, or) = time_it(3, || {
        workloads::go_startup(workloads::jit_regions(), Some(THR), true)
    });
    assert_eq!(
        or,
        workloads::GO_HELLO_OUT,
        "go region-bg output != expected"
    );
    println!(
        "{:<11} {:>10} {:>14} {:>14} {:>14}",
        "go-startup",
        ms(go_eager.as_nanos() as u64),
        speed(go_eager, go_inline),
        speed(go_eager, go_bg),
        speed(go_eager, go_rbg),
    );

    // hotloop: a long, MULTI-BLOCK warm loop — the case regions are meant to win
    // (BGT-6). Long enough that the region's one-time compile amortizes.
    const HOT_N: u32 = 20_000_000;
    let (h_eager, he) = time_it(3, || {
        workloads::guest_hotloop(workloads::jit(), TierCfg::EAGER, HOT_N).0
    });
    let (h_inline, hi) = time_it(3, || {
        workloads::guest_hotloop(workloads::jit(), TierCfg::tier(THR), HOT_N).0
    });
    assert_eq!(hi, he, "hotloop inline output != eager");
    let (h_bg, hb) = time_it(3, || {
        workloads::guest_hotloop(workloads::jit(), TierCfg::bg(THR), HOT_N).0
    });
    assert_eq!(hb, he, "hotloop bg output != eager");
    let (h_rbg, hr) = time_it(3, || {
        workloads::guest_hotloop(workloads::jit_regions(), TierCfg::bg(THR), HOT_N).0
    });
    assert_eq!(hr, he, "hotloop region-bg output != eager");
    println!(
        "{:<11} {:>10} {:>14} {:>14} {:>14}",
        "hotloop",
        ms(h_eager.as_nanos() as u64),
        speed(h_eager, h_inline),
        speed(h_eager, h_bg),
        speed(h_eager, h_rbg),
    );

    println!("\n(cell = time (speedup vs eager); >1x means faster than eager JIT)");
}

/// Print each workload's golden output (interpreter leg) — the value baked into its
/// `expect` field. Handy when a kernel's deterministic result changes and the const
/// needs re-seeding, and as a quick "what does this produce" check.
fn dump() {
    use workloads::TierCfg;
    for wl in workloads::all() {
        let (out, _) = (wl.guest)(workloads::interp(), TierCfg::EAGER);
        let ok = if out == wl.expect {
            "==expect"
        } else {
            "!!DIFF"
        };
        println!("{:<10} {:<12} {ok}", wl.name, String::from_utf8_lossy(&out));
    }
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

/// Print the last `N` records' jit/interp ratio per workload — the commit-series
/// view (perf-bench v2 PB-4), so drift is visible rather than a single-point surprise.
fn trend() {
    let n: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let recs = report::all_records();
    if recs.is_empty() {
        println!("no records yet. Run `record`.");
        return;
    }
    let recent = &recs[recs.len().saturating_sub(n)..];
    // Column set = the workloads of the newest record.
    let names: Vec<String> = recent
        .last()
        .unwrap()
        .workloads
        .iter()
        .map(|w| w.name.clone())
        .collect();
    print!("{:<10} {:<7}", "commit", "qual");
    for name in &names {
        print!(" {name:>10}");
    }
    println!("  subject");
    for r in recent {
        print!(
            "{:<10} {:<7}",
            r.commit_short,
            r.quality
                .as_deref()
                .unwrap_or(if r.dirty { "dirty" } else { "?" })
        );
        for name in &names {
            let cell = r
                .workloads
                .iter()
                .find(|w| &w.name == name)
                .map(|w| format!("{:.2}x", w.jit_vs_interp()))
                .unwrap_or_else(|| "-".into());
            print!(" {cell:>10}");
        }
        println!("  {}", r.subject);
    }
}
