//! Long-running AVX2 fuzz driver (task-259..264 sweep). `#[ignore]`d — run explicitly:
//!   FUZZ_SECONDS=7200 cargo test --release -p x86jit-tests --features unicorn \
//!       --test fuzz_avx -- --ignored --nocapture
//!
//! Generates random programs that contain at least one VEX/AVX2 op from the new sweep, then
//! checks two oracles per program. JIT vs interpreter: any divergence is a codegen bug (the
//! interp is the JIT's oracle). Real host CPU (NativeOracle) vs interpreter: any divergence
//! is a semantics bug (interp wrong vs hardware); the ground truth for VEX.
//! Divergences are shrunk, deduplicated by (leg, op-signature), and appended to a log — the
//! run does NOT stop on the first failure, so one multi-hour pass surfaces every distinct bug.
//!
//! Env: FUZZ_SECONDS (default 3600), FUZZ_LEN (default 12), FUZZ_START (default 1),
//!      FUZZ_LOG (default fuzz-avx-findings.log).

use std::collections::HashSet;
use std::io::Write as _;
use std::time::{Duration, Instant};

use x86jit_core::{GuestCpuFeatures, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::compare::compare;
use x86jit_tests::fuzz::{dontcare_flags, gen, shrink, FuzzInsn, Prog};
use x86jit_tests::oracle::{run_with_backend_mode, RunOutcome};

fn interp(p: &Prog) -> RunOutcome {
    run_with_backend_mode(
        &p.input(),
        Box::new(InterpreterBackend),
        GuestCpuFeatures::default(),
        p.mode,
    )
}
fn jit(p: &Prog) -> RunOutcome {
    run_with_backend_mode(
        &p.input(),
        Box::new(JitBackend::new()),
        GuestCpuFeatures::default(),
        p.mode,
    )
}

#[cfg(target_arch = "x86_64")]
fn run_native_opt(p: &Prog) -> Option<RunOutcome> {
    x86jit_tests::native::run_native(&p.input())
}
#[cfg(not(target_arch = "x86_64"))]
fn run_native_opt(_p: &Prog) -> Option<RunOutcome> {
    None
}

fn vex_sig(p: &Prog) -> String {
    let mut ops: Vec<u8> = p
        .insns
        .iter()
        .filter_map(|i| match i {
            FuzzInsn::VVex { op, .. } => Some(*op % 63),
            _ => None,
        })
        .collect();
    ops.sort_unstable();
    ops.dedup();
    ops.iter()
        .map(|o| o.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn has_vex(p: &Prog) -> bool {
    p.insns.iter().any(|i| matches!(i, FuzzInsn::VVex { .. }))
}

/// A legacy-SSE *vector* op. x86jit models these as clearing bits 255:128 (`set_vec`
/// zero-extends), whereas real hardware PRESERVES the upper on legacy SSE. With a seeded
/// (dirty) ymm upper, that documented model choice diverges from the NativeOracle — noise
/// that is NOT a sweep bug — so the native leg skips any program containing one. The
/// JIT-vs-interp leg still runs (jit == interp regardless of the upper-clear model).
fn has_legacy_vec(p: &Prog) -> bool {
    p.insns.iter().any(|i| {
        matches!(
            i,
            FuzzInsn::VBin { .. }
                | FuzzInsn::VNew { .. }
                | FuzzInsn::VShiftImm { .. }
                | FuzzInsn::VShuf { .. }
                | FuzzInsn::VMovMask { .. }
        )
    })
}

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

#[test]
#[ignore = "multi-hour fuzz driver; run explicitly with --ignored"]
fn fuzz_avx() {
    let secs = env_u64("FUZZ_SECONDS", 3600);
    let len = env_u64("FUZZ_LEN", 12) as usize;
    let start = env_u64("FUZZ_START", 1);
    let log_path = std::env::var("FUZZ_LOG").unwrap_or_else(|_| "fuzz-avx-findings.log".into());
    let mut log = std::fs::File::create(&log_path).expect("create log");

    let deadline = Instant::now() + Duration::from_secs(secs);
    let native_ok = cfg!(target_arch = "x86_64");

    let mut seen: HashSet<String> = HashSet::new();
    let (mut checked, mut jit_hits, mut native_hits, mut native_run) = (0u64, 0u64, 0u64, 0u64);
    let mut seed = start;

    macro_rules! record {
        ($leg:expr, $seed:expr, $min:expr, $diff:expr) => {{
            let key = format!("{}:{}", $leg, vex_sig(&$min));
            if seen.insert(key) {
                let msg = format!(
                    "=== {} divergence (seed {}) ===\nops: {:#?}\n{}\n\n",
                    $leg, $seed, $min.insns, $diff
                );
                print!("{msg}");
                let _ = log.write_all(msg.as_bytes());
                let _ = log.flush();
            }
        }};
    }

    while Instant::now() < deadline {
        let prog = gen(seed, len);
        seed += 1;
        if !has_vex(&prog) {
            continue; // focus the budget on the new VEX ops
        }
        checked += 1;

        let i = interp(&prog);
        let j = jit(&prog);
        if let Some(d) = compare(&i, &j, &[]) {
            let mut div = |p: &Prog| compare(&interp(p), &jit(p), &[]).is_some();
            let min = shrink(&prog, &mut div);
            let dd = compare(&interp(&min), &jit(&min), &[]).unwrap_or(d);
            jit_hits += 1;
            record!("JIT-vs-interp", prog.seed, min, dd);
        }

        if native_ok && !has_legacy_vec(&prog) {
            if let Some(nat) = run_native_opt(&prog) {
                native_run += 1;
                if let Some(d) = compare(&nat, &i, &dontcare_flags(&prog)) {
                    let mut div = |p: &Prog| {
                        run_native_opt(p)
                            .map(|n| compare(&n, &interp(p), &dontcare_flags(p)).is_some())
                            .unwrap_or(false)
                    };
                    let min = shrink(&prog, &mut div);
                    let dd = run_native_opt(&min)
                        .and_then(|n| compare(&n, &interp(&min), &dontcare_flags(&min)))
                        .unwrap_or(d);
                    native_hits += 1;
                    record!("native-vs-interp", prog.seed, min, dd);
                }
            }
        }

        if checked % 20_000 == 0 {
            let left = deadline.saturating_duration_since(Instant::now()).as_secs();
            eprintln!(
                "[{left}s left] checked={checked} native_run={native_run} distinct_bugs={} (jit={jit_hits} native={native_hits}) seed={seed}",
                seen.len()
            );
        }
    }

    let summary = format!(
        "\n=== fuzz-avx done ===\nchecked(with-vex)={checked} native_run={native_run} distinct_bugs={} jit_hits={jit_hits} native_hits={native_hits} last_seed={seed}\n",
        seen.len()
    );
    print!("{summary}");
    let _ = log.write_all(summary.as_bytes());
}
