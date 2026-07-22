//! Result records: JSON stored per commit under `bench/history/`, plus the git /
//! host metadata that makes cross-commit comparison meaningful.

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// A timing's distribution summary over the kept samples (perf-bench v2, doc-29
/// PB-1). `min` is the intrinsic-cost estimate (noise only adds time); `median` is
/// the gate's robust reference; `mad` (median absolute deviation) is the noise band
/// the noise-aware gate compares a delta against. Old `history/` records predate this
/// (only `*_ns` min fields) — [`WlResult::interp`]/`jit_cold`/`native` synthesize a
/// degenerate `Stat` (min=median, mad=0) from them so the series still loads.
#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug)]
pub struct Stat {
    pub min_ns: u64,
    pub median_ns: u64,
    pub mad_ns: u64,
    pub n: u32,
}

impl Stat {
    /// A degenerate stat from a single number (a pre-v2 min-only record).
    pub fn from_min(min_ns: u64) -> Self {
        Stat {
            min_ns,
            median_ns: min_ns,
            mad_ns: 0,
            n: 1,
        }
    }
    /// Noise band as a fraction of the median (MAD/median), for the noise-aware gate.
    pub fn rel_noise(&self) -> f64 {
        if self.median_ns == 0 {
            0.0
        } else {
            self.mad_ns as f64 / self.median_ns as f64
        }
    }
}

/// One benchmark run's full result set, keyed by the commit it was taken at.
/// `host` + `dirty` guard comparisons: timings only compare on the same machine, and
/// a dirty tree means the numbers don't belong to the recorded commit. `loadavg1` +
/// `quality` (perf-bench v2) let the gate discount a record taken under host load.
#[derive(Serialize, Deserialize)]
pub struct Record {
    pub commit: String,
    pub commit_short: String,
    pub subject: String,
    pub dirty: bool,
    pub host: String,
    pub cpu: String,
    pub timestamp_unix: u64,
    pub iters: u32,
    pub workloads: Vec<WlResult>,
    /// 1-minute load average when recorded (v2; `None` on pre-v2 records).
    #[serde(default)]
    pub loadavg1: Option<f64>,
    /// "clean" | "loaded" | "dirty" (v2; `None` on pre-v2 records).
    #[serde(default)]
    pub quality: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct WlResult {
    pub name: String,
    pub kind: String,
    /// Min native subprocess wall-clock, if a native binary exists.
    pub native_ns: Option<u64>,
    pub interp_ns: u64,
    pub jit_ns: u64,
    // Fast-dispatch counters from the JIT run (evidence of what fires).
    pub chained: u64,
    pub ibtc_filled: u64,
    pub fast_hits: u64,
    pub misses: u64,
    // --- perf-bench v2 (doc-29). Optional so pre-v2 records still deserialize. ---
    /// Full distribution for interp / JIT-cold / native. `None` on pre-v2 records.
    #[serde(default)]
    pub interp_stat: Option<Stat>,
    #[serde(default)]
    pub jit_stat: Option<Stat>,
    #[serde(default)]
    pub native_stat: Option<Stat>,
    /// Compilation time inside the JIT-cold run (PB-2). `None` until PB-2 lands.
    #[serde(default)]
    pub compile_ns: Option<u64>,
    /// Guest instructions executed in the JIT run (task-281), when `X86JIT_ICOUNT=1`.
    /// With `jit_ns - compile_ns` this gives guest MIPS — the per-instruction cost
    /// task-282 is about. `None`/0 when the accounting was not enabled.
    #[serde(default)]
    pub executed: Option<u64>,
    /// Calls out of compiled code into interpreter helpers, and the busiest one
    /// (task-282). `None` on records from before the counter existed.
    #[serde(default)]
    pub helper_calls: Option<u64>,
    #[serde(default)]
    pub top_helper: Option<String>,
    /// Wall-clock in the tiered / background-tiered deployment modes (tiering track):
    /// interpret-until-hot then compile (inline / on a worker). `None` on records that
    /// didn't measure the modes (the `gate` skips them for speed; pre-v2 records).
    #[serde(default)]
    pub tier_stat: Option<Stat>,
    #[serde(default)]
    pub bg_stat: Option<Stat>,
    /// Background tier-up with a region-forming backend (BGT-6): hot loops tier up to
    /// superblock regions off-thread. `None` on the fast `gate` path and in old records.
    #[serde(default)]
    pub region_bg_stat: Option<Stat>,
}

impl WlResult {
    pub fn jit_vs_interp(&self) -> f64 {
        self.interp_ns as f64 / self.jit_ns as f64
    }
    pub fn jit_vs_native(&self) -> Option<f64> {
        self.native_ns.map(|n| self.jit_ns as f64 / n as f64)
    }
    /// The interp distribution (v2), or a degenerate stat from the pre-v2 min.
    pub fn interp(&self) -> Stat {
        self.interp_stat
            .unwrap_or_else(|| Stat::from_min(self.interp_ns))
    }
    /// The JIT-cold distribution (compile + execute).
    pub fn jit_cold(&self) -> Stat {
        self.jit_stat.unwrap_or_else(|| Stat::from_min(self.jit_ns))
    }
    /// The native distribution, if a native reference exists.
    pub fn native(&self) -> Option<Stat> {
        self.native_stat
            .or_else(|| self.native_ns.map(Stat::from_min))
    }
    /// Compilation time inside the JIT-cold run (perf-bench v2 PB-2); 0 if not
    /// recorded (pre-v2, or the interpreter).
    pub fn compile(&self) -> u64 {
        self.compile_ns.unwrap_or(0)
    }
    /// Steady-state JIT execution — JIT-cold minus compilation (PB-2). The number
    /// that matters for a long-running guest; for a compile-dominated one-shot
    /// (`sqlite`/`lua`) it is a small fraction of `jit_cold`. `None` when compile
    /// time wasn't recorded (a pre-v2 record can't be split).
    pub fn run(&self) -> Option<Stat> {
        self.compile_ns.map(|c| {
            let cold = self.jit_cold();
            Stat {
                min_ns: cold.min_ns.saturating_sub(c),
                median_ns: cold.median_ns.saturating_sub(c),
                mad_ns: cold.mad_ns,
                n: cold.n,
            }
        })
    }
    /// `t`'s median as a multiple of native (perf-bench v2 PB-3), if a native
    /// reference exists. `interp_vs_native` / `run_vs_native` are the honest "how far
    /// off native" numbers; `run` (compile amortized) is the headline for the JIT.
    pub fn vs_native(&self, t: Stat) -> Option<f64> {
        self.native()
            .filter(|n| n.median_ns > 0)
            .map(|n| t.median_ns as f64 / n.median_ns as f64)
    }
    pub fn interp_vs_native(&self) -> Option<f64> {
        self.vs_native(self.interp())
    }
    pub fn jit_cold_vs_native(&self) -> Option<f64> {
        self.vs_native(self.jit_cold())
    }
    pub fn run_vs_native(&self) -> Option<f64> {
        self.run().and_then(|r| self.vs_native(r))
    }
}

/// `bench/history/` under the repo root (one dir up from this crate).
pub fn history_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../bench/history"))
}

pub fn record_path(commit_short: &str) -> PathBuf {
    history_dir().join(format!("{commit_short}.json"))
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn head_full() -> String {
    git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into())
}

pub fn head_short() -> String {
    git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into())
}

pub fn head_subject() -> String {
    git(&["log", "-1", "--format=%s"]).unwrap_or_default()
}

/// Resolve any git ref (short sha, "HEAD", branch) to the short sha we key files
/// by. Falls back to the input so a raw short sha still works offline.
pub fn resolve_short(reff: &str) -> String {
    git(&["rev-parse", "--short", reff]).unwrap_or_else(|| reff.to_string())
}

/// A dirty working tree means recorded timings don't belong to the commit.
pub fn is_dirty() -> bool {
    git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

pub fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// First `model name` line from /proc/cpuinfo, for context in the record.
pub fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 1-minute load average from `/proc/loadavg` (Linux), or `None` off-Linux.
pub fn loadavg1() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Logical CPU count (for the load-quality threshold).
pub fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// A record's quality tag (perf-bench v2, PB-1): `dirty` tree, or `loaded` when the
/// 1-minute load exceeds half the core count (timings then carry scheduling noise),
/// else `clean`. Only a `clean` record is eligible as a rolling-median gate reference.
pub fn quality(dirty: bool, loadavg1: Option<f64>) -> String {
    if dirty {
        return "dirty".into();
    }
    match loadavg1 {
        Some(l) if l > num_cpus() as f64 * 0.5 => "loaded".into(),
        _ => "clean".into(),
    }
}

pub fn save(rec: &Record) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(history_dir())?;
    let path = record_path(&rec.commit_short);
    let json = serde_json::to_string_pretty(rec).expect("serialize record");
    std::fs::write(&path, json + "\n")?;
    Ok(path)
}

pub fn load(commit_short: &str) -> std::io::Result<Record> {
    let path = record_path(commit_short);
    let json = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&json).expect("parse record"))
}

/// The accepted-performance reference the pre-push `gate` measures against.
/// Committed at repo root under `bench/`, moved only by `record` (an explicit
/// "accept this as the new baseline" action).
pub fn baseline_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../bench/baseline.json"
    ))
}

pub fn load_baseline() -> Option<Record> {
    let json = std::fs::read_to_string(baseline_path()).ok()?;
    serde_json::from_str(&json).ok()
}

pub fn save_baseline(rec: &Record) -> std::io::Result<PathBuf> {
    let path = baseline_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(rec).expect("serialize baseline");
    std::fs::write(&path, json + "\n")?;
    Ok(path)
}

/// The committed comparison doc, listed by Backlog.md (`backlog/docs/performance.md`).
pub fn perf_md_path() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../backlog/docs/performance.md"
    ))
}

/// Render `rec` against `prev` (the baseline before this record) into a readable
/// Backlog.md doc: per workload, interp/JIT/native minima, the JIT-vs-interp
/// speedup, and the signed delta this snapshot introduces. `prev == None` (first
/// baseline) prints an em dash for the deltas.
pub fn write_performance_md(rec: &Record, prev: Option<&Record>) -> std::io::Result<PathBuf> {
    fn ms(ns: u64) -> String {
        format!("{:.2}ms", ns as f64 / 1e6)
    }
    fn delta_cell(prev: Option<&Record>, name: &str, cur: u64, jit: bool) -> String {
        let Some(p) = prev.and_then(|p| p.workloads.iter().find(|w| w.name == name)) else {
            return "—".into();
        };
        let base = if jit { p.jit_ns } else { p.interp_ns };
        if base == 0 {
            return "—".into();
        }
        let d = (cur as f64 - base as f64) / base as f64 * 100.0;
        // Faster is better: ▼ (down = quicker), ▲ = slower.
        let arrow = if d < -0.05 {
            "▼"
        } else if d > 0.05 {
            "▲"
        } else {
            "•"
        };
        format!("{arrow} {}{:.1}%", if d <= 0.0 { "" } else { "+" }, d)
    }

    let mut s = String::new();
    s.push_str(
        "---\nid: doc-26\ntitle: 'Performance — native vs interpreter vs JIT'\ntype: other\n\
         created_date: '2026-07-06 11:25'\n---\n\n",
    );
    // A timing cell: median ms with the noise band (MAD as a % of the median) — the
    // number the noise-aware gate reasons about (perf-bench v2, PB-1).
    fn stat_cell(s: Stat) -> String {
        format!("{} ±{:.0}%", ms(s.median_ns), s.rel_noise() * 100.0)
    }
    s.push_str("# Performance\n\n");
    s.push_str(&format!(
        "Median (± MAD noise band) timings, **generated by `cargo run -p x86jit-bench --release -- record`** — do NOT \
         edit by hand. Baseline = `{}` \"{}\", host `{}`, quality `{}` (loadavg {:.2}), {} iters. `Δ` columns \
         compare this snapshot's min to the *previous* baseline (▼ faster, ▲ slower). The pre-push `gate` blocks a \
         push whose jit/interp ratio regresses past `max(X86JIT_PERF_THRESHOLD` (default 10%)`, measured noise band)` \
         vs `bench/baseline.json`, unless `X86JIT_ALLOW_PERF_REGRESSION=1`.\n\n",
        rec.commit_short, rec.subject, rec.host,
        rec.quality.as_deref().unwrap_or("?"),
        rec.loadavg1.unwrap_or(0.0),
        rec.iters
    ));
    // A "×native" ratio cell (PB-3), or "-" with no native reference.
    fn xnat(r: Option<f64>) -> String {
        r.map(|v| format!("{v:.1}x")).unwrap_or_else(|| "-".into())
    }
    s.push_str(
        "| workload | kind | native | interp | jit-cold | compile | run | jit/int | interp/nat | jit/nat | run/nat | Δ interp | Δ jit |\n",
    );
    s.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    for w in &rec.workloads {
        let nat = w.native().map(stat_cell).unwrap_or_else(|| "-".into());
        // compile / run split (PB-2): `run` is the steady-state execute (cold −
        // compile) — dashes on a pre-v2 record that has no compile time.
        let compile = if w.compile_ns.is_some() {
            ms(w.compile())
        } else {
            "-".into()
        };
        let run = w.run().map(stat_cell).unwrap_or_else(|| "-".into());
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {:.2}x | {} | {} | {} | {} | {} |\n",
            w.name,
            w.kind,
            nat,
            stat_cell(w.interp()),
            stat_cell(w.jit_cold()),
            compile,
            run,
            w.jit_vs_interp(),
            xnat(w.interp_vs_native()),
            xnat(w.jit_cold_vs_native()),
            xnat(w.run_vs_native()),
            delta_cell(prev, &w.name, w.interp_ns, false),
            delta_cell(prev, &w.name, w.jit_ns, true),
        ));
    }
    // Tiering table (tiering track): wall-clock across the deployment modes. Shown
    // only if this record measured them (`record` does; the `gate` skips them). For a
    // compile-dominated one-shot the tiered modes are dramatically faster than eager
    // (only hot blocks compile); for a hot loop they converge (it compiles anyway).
    if rec.workloads.iter().any(|w| w.tier_stat.is_some()) {
        s.push_str(
            "\n## Tiering — wall-clock by mode\n\n\
             `eager` compiles every block on first execution; `tier` interprets a block \
             until it is hot (50 runs) then compiles it inline; `bg` compiles hot blocks \
             on a worker thread (`x86jit-run` ships `tier`); `region-bg` (BGT-6, opt-in) \
             tiers a hot loop up to a background-compiled superblock region — a win only \
             on long multi-block warm loops (`hotloop`), a loss on one-shot workloads. \
             `best↓` is the fastest mode's speedup over `eager`.\n\n",
        );
        s.push_str(
            "| workload | kind | interp | eager | tier | bg | region-bg | native | best↓ vs eager |\n",
        );
        s.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|\n");
        for w in &rec.workloads {
            let med = |o: Option<Stat>| o.map(|s| ms(s.median_ns)).unwrap_or_else(|| "-".into());
            let eager = w.jit_cold().median_ns as f64;
            // Fastest of eager/tier/bg/region-bg, as a speedup over eager.
            let best = [
                w.jit_cold().median_ns,
                w.tier_stat.map(|s| s.median_ns).unwrap_or(u64::MAX),
                w.bg_stat.map(|s| s.median_ns).unwrap_or(u64::MAX),
                w.region_bg_stat.map(|s| s.median_ns).unwrap_or(u64::MAX),
            ]
            .into_iter()
            .min()
            .unwrap();
            let best_cell = if best > 0 && best < u64::MAX {
                format!("{:.1}x", eager / best as f64)
            } else {
                "-".into()
            };
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                w.name,
                w.kind,
                stat_cell(w.interp()),
                stat_cell(w.jit_cold()),
                med(w.tier_stat),
                med(w.bg_stat),
                med(w.region_bg_stat),
                w.native().map(stat_cell).unwrap_or_else(|| "-".into()),
                best_cell,
            ));
        }
    }
    s.push_str(&format!(
        "\n_Host CPU: {}. Recorded at unix {}._\n",
        rec.cpu, rec.timestamp_unix
    ));

    let path = perf_md_path();
    std::fs::write(&path, s)?;
    Ok(path)
}

/// Every stored record (sorted by timestamp ascending), for `list`/`trend`.
pub fn all_records() -> Vec<Record> {
    let mut recs: Vec<Record> = std::fs::read_dir(history_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    recs.sort_by_key(|r: &Record| r.timestamp_unix);
    recs
}

/// The most recent `k` **clean** records on `host` (perf-bench v2 PB-4), oldest
/// first — the rolling window the gate reduces to a reference. A `loaded`/`dirty`
/// record is excluded (its noisy timings would poison the reference). Fewer than `k`
/// clean records just returns what exists (the gate then falls back if too few).
pub fn clean_recent(host: &str, k: usize) -> Vec<Record> {
    let mut recs: Vec<Record> = all_records()
        .into_iter()
        .filter(|r| r.host == host && !r.dirty && r.quality.as_deref() != Some("loaded"))
        .collect();
    let n = recs.len();
    recs.split_off(n.saturating_sub(k))
}

/// Median of `xs` (sorts a copy). `None` if empty.
pub fn median(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

/// Median absolute deviation of `xs` about `med`.
pub fn mad(xs: &[f64], med: f64) -> f64 {
    let dev: Vec<f64> = xs.iter().map(|x| (x - med).abs()).collect();
    median(&dev).unwrap_or(0.0)
}
