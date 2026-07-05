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
    git(&["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false)
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
