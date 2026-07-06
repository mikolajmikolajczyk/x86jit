//! Result records: JSON stored per commit under `bench/history/`, plus the git /
//! host metadata that makes cross-commit comparison meaningful.

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// One benchmark run's full result set, keyed by the commit it was taken at.
/// Timings are median-of-N nanoseconds. `host` + `dirty` guard comparisons:
/// timings only compare on the same machine, and a dirty tree means the numbers
/// don't belong to the recorded commit.
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
}

#[derive(Serialize, Deserialize, Clone)]
pub struct WlResult {
    pub name: String,
    pub kind: String,
    /// Median native subprocess wall-clock, if a native binary exists.
    pub native_ns: Option<u64>,
    pub interp_ns: u64,
    pub jit_ns: u64,
    // Fast-dispatch counters from the JIT run (evidence of what fires).
    pub chained: u64,
    pub ibtc_filled: u64,
    pub fast_hits: u64,
    pub misses: u64,
}

impl WlResult {
    pub fn jit_vs_interp(&self) -> f64 {
        self.interp_ns as f64 / self.jit_ns as f64
    }
    pub fn jit_vs_native(&self) -> Option<f64> {
        self.native_ns.map(|n| self.jit_ns as f64 / n as f64)
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
/// Backlog.md doc: per workload, interp/JIT/native medians, the JIT-vs-interp
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
    s.push_str("# Performance\n\n");
    s.push_str(&format!(
        "Median timings, **generated by `cargo run -p x86jit-bench --release -- record`** — do NOT \
         edit by hand. Baseline = `{}` \"{}\", host `{}` ({} iters). `Δ` columns compare this \
         snapshot to the *previous* baseline (▼ faster, ▲ slower). The pre-push `gate` blocks a \
         push whose interp or JIT time regresses > `X86JIT_PERF_THRESHOLD` (default 10%) vs \
         `bench/baseline.json`, unless `X86JIT_ALLOW_PERF_REGRESSION=1`.\n\n",
        rec.commit_short, rec.subject, rec.host, rec.iters
    ));
    s.push_str("| workload | kind | native | interp | jit | jit/int | Δ interp | Δ jit |\n");
    s.push_str("|---|---|---:|---:|---:|---:|---:|---:|\n");
    for w in &rec.workloads {
        let nat = w.native_ns.map(ms).unwrap_or_else(|| "-".into());
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2}x | {} | {} |\n",
            w.name,
            w.kind,
            nat,
            ms(w.interp_ns),
            ms(w.jit_ns),
            w.jit_vs_interp(),
            delta_cell(prev, &w.name, w.interp_ns, false),
            delta_cell(prev, &w.name, w.jit_ns, true),
        ));
    }
    s.push_str(&format!(
        "\n_Host CPU: {}. Recorded at unix {}._\n",
        rec.cpu, rec.timestamp_unix
    ));

    let path = perf_md_path();
    std::fs::write(&path, s)?;
    Ok(path)
}

/// Every stored record (sorted by timestamp), for `list`.
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
