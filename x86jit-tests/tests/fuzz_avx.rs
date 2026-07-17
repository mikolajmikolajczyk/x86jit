//! AVX2 VEX differential fuzz drivers (task-259..264 sweep, refactored task-267).
//!
//! The campaign machinery — two legs (JIT-vs-interp + native-vs-interp), shrink, dedup, and
//! per-op coverage — now lives in the library (`x86jit_tests::fuzz::run_campaign`) and backs
//! the `cargo xfuzz` CLI. These are just thin drivers:
//!
//!   * `fuzz_avx_smoke` — a fast (<=5s) nextest-visible run that exercises `run_campaign`.
//!   * `fuzz_avx` — the `#[ignore]`d long driver (prefer `cargo xfuzz`; kept for CI/env use).
//!
//! Long run (equivalent to `cargo xfuzz --secs 7200 --len 12`):
//!   FUZZ_SECONDS=7200 cargo test --release -p x86jit-tests --test fuzz_avx -- --ignored --nocapture

use x86jit_tests::fuzz::{all_ops, print_report, run_campaign, CampaignCfg};

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

/// Fast machinery check: a couple of seconds over the full pool, asserting the campaign made
/// progress. Exercises both legs (JIT + native where available) under nextest.
#[test]
fn fuzz_avx_smoke() {
    let cfg = CampaignCfg {
        secs: 2,
        len: 8,
        start_seed: 1,
        single: None,
        vex_ops: all_ops(),
        log_path: None,
        repro_prefix: "cargo xfuzz".into(),
        status: false,
        quiet: true,
    };
    let report = run_campaign(&cfg);
    assert!(report.checked > 0, "campaign checked no programs");
    // Some VEX op must have been generated and counted.
    assert!(
        report.cov.iter().any(|c| c.generated > 0),
        "no per-op coverage recorded"
    );
}

#[test]
#[ignore = "multi-hour fuzz driver; prefer `cargo xfuzz`, or run with --ignored"]
fn fuzz_avx() {
    let cfg = CampaignCfg {
        secs: env_u64("FUZZ_SECONDS", 3600),
        len: env_u64("FUZZ_LEN", 12) as usize,
        start_seed: env_u64("FUZZ_START", 1),
        single: None,
        vex_ops: all_ops(),
        log_path: Some(
            std::env::var("FUZZ_LOG")
                .unwrap_or_else(|_| "fuzz-avx-findings.log".into())
                .into(),
        ),
        repro_prefix: "cargo xfuzz".into(),
        status: true,
        quiet: false,
    };
    let report = run_campaign(&cfg);
    print_report(&report);
}
