//! Differential fuzzing (M4, testing.md §7). Random valid programs, two engines,
//! any state divergence is a bug — shrunk to a minimal reproducer, seed recorded,
//! and auto-saved to `vectors/found/` before the test fails.
//!
//! - `jit_matches_interp` (default): JIT vs interpreter, exact match required —
//!   the JIT mirrors the interpreter, so any divergence is a codegen bug.
//! - `unicorn_matches_interp` (`--features unicorn`): the lift/interp vs the
//!   Unicorn (real-CPU) truth, masking the flags each program leaves architecturally
//!   undefined (computed per program from the instruction semantics).
//! - `native_matches_interp` (x86-64/Linux): the interp vs the **real host CPU**
//!   (NativeOracle, task-186) — the only leg that decodes VEX/EVEX faithfully, so it
//!   validates BMI/AVX *semantics* against hardware, not just JIT-vs-interp codegen.

use std::path::PathBuf;

use x86jit_core::{GuestCpuFeatures, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::compare::compare;
use x86jit_tests::fuzz::{gen, gen32, shrink, Prog};
use x86jit_tests::oracle::{run_with_backend, run_with_backend_mode, RunOutcome};
use x86jit_tests::vector::{Expectation, FlagName, MemChunk, TestVector};

fn interp(prog: &Prog) -> RunOutcome {
    run_with_backend_mode(
        &prog.input(),
        Box::new(InterpreterBackend),
        GuestCpuFeatures::default(),
        prog.mode,
    )
}

fn jit(prog: &Prog) -> RunOutcome {
    run_with_backend_mode(
        &prog.input(),
        Box::new(JitBackend::new()),
        GuestCpuFeatures::default(),
        prog.mode,
    )
}

fn jit_superblocks(prog: &Prog) -> RunOutcome {
    let caps = x86jit_core::RegionCaps {
        max_blocks: 16,
        max_icount: 256,
    };
    run_with_backend(&prog.input(), Box::new(JitBackend::with_superblocks(caps)))
}

/// The superblock JIT must match the interpreter exactly too (superblocks M5-T3).
#[test]
fn jit_superblocks_matches_interp() {
    for seed in 1..600u64 {
        let prog = gen(seed, 12);
        let i = interp(&prog);
        let j = jit_superblocks(&prog);
        assert!(
            compare(&i, &j, &[]).is_none(),
            "superblock JIT diverges from interpreter (seed {seed}):\n{:#?}",
            prog.insns
        );
    }
}

#[test]
fn jit_matches_interp() {
    for seed in 1..600u64 {
        let prog = gen(seed, 12);
        let i = interp(&prog);
        let j = jit(&prog);
        if compare(&i, &j, &[]).is_some() {
            let mut diverges = |p: &Prog| compare(&interp(p), &jit(p), &[]).is_some();
            let minimal = shrink(&prog, &mut diverges);
            let path = save_found(&minimal, &interp(&minimal), &[FlagName::Af]);
            let d = compare(&interp(&minimal), &jit(&minimal), &[]).unwrap();
            panic!(
                "JIT diverges from interpreter (seed {seed}, saved {}):\n{:#?}\n{d}",
                path.display(),
                minimal.insns
            );
        }
    }
}

/// The JIT must match the interpreter for 32-bit (`CpuMode::Compat32`) programs too
/// (task-197.5). `gen32` restricts generation to mode-neutral / genuinely-32-bit
/// forms (no 64-bit operands, no r8–r15, inc/dec 0x40–0x4F), so any divergence here
/// is a codegen bug on the 32-bit lane, not a missing 197.2/197.3 semantic.
#[test]
fn jit_matches_interp_32() {
    for seed in 1..600u64 {
        let prog = gen32(seed, 12);
        let i = interp(&prog);
        let j = jit(&prog);
        if compare(&i, &j, &[]).is_some() {
            let mut diverges = |p: &Prog| compare(&interp(p), &jit(p), &[]).is_some();
            let minimal = shrink(&prog, &mut diverges);
            let d = compare(&interp(&minimal), &jit(&minimal), &[]).unwrap();
            panic!(
                "32-bit JIT diverges from interpreter (seed {seed}):\n{:#?}\n{d}",
                minimal.insns
            );
        }
    }
}

/// The 32-bit lift/interp vs Unicorn `UC_MODE_32` (task-197.5, AC#2). `gen32` emits
/// only mode-neutral / genuinely-32-bit forms — the address-wrap, 67h, and stack-
/// width cases that 197.2/197.3 own are deliberately NOT generated, so this lane
/// stays green on pure 197.1 plumbing while still exercising the 0x40–0x4F inc/dec
/// short forms, 8/16/32-bit arithmetic, shifts, and SSE under the 32-bit decoder.
#[cfg(feature = "unicorn")]
#[test]
fn unicorn_matches_interp_32() {
    use x86jit_tests::fuzz::dontcare_flags;
    use x86jit_tests::oracle::Oracle;
    use x86jit_tests::unicorn::UnicornOracle32;

    for seed in 1..300u64 {
        let prog = gen32(seed, 12);
        let interp_out = interp(&prog);
        let uni = UnicornOracle32.run(&prog.input());
        if compare(&uni, &interp_out, &dontcare_flags(&prog)).is_some() {
            let mut diverges = |p: &Prog| {
                compare(
                    &UnicornOracle32.run(&p.input()),
                    &interp(p),
                    &dontcare_flags(p),
                )
                .is_some()
            };
            let minimal = shrink(&prog, &mut diverges);
            let d = compare(
                &UnicornOracle32.run(&minimal.input()),
                &interp(&minimal),
                &dontcare_flags(&minimal),
            )
            .unwrap();
            panic!(
                "32-bit interpreter diverges from Unicorn UC_MODE_32 (seed {seed}):\n{:#?}\n{d}",
                minimal.insns
            );
        }
    }
}

#[cfg(feature = "unicorn")]
#[test]
fn unicorn_matches_interp() {
    use x86jit_tests::fuzz::dontcare_flags;
    use x86jit_tests::oracle::Oracle;
    use x86jit_tests::unicorn::UnicornOracle;

    // Flags left architecturally undefined by the program's instructions (MUL's
    // SF/ZF/AF/PF, a shift's OF for count≠1, bt's non-CF flags, …) can't be compared
    // against real hardware — mask exactly those, computed per program.
    for seed in 1..300u64 {
        let prog = gen(seed, 12);
        // Skip programs containing ops Unicorn's QEMU can't decode (SSSE3 ph*); the
        // NativeOracle and JIT-vs-interp legs cover those.
        if x86jit_tests::fuzz::unicorn_incompatible(&prog) {
            continue;
        }
        let interp_out = interp(&prog);
        let uni = UnicornOracle.run(&prog.input());
        if compare(&uni, &interp_out, &dontcare_flags(&prog)).is_some() {
            let mut diverges = |p: &Prog| {
                compare(
                    &UnicornOracle.run(&p.input()),
                    &interp(p),
                    &dontcare_flags(p),
                )
                .is_some()
            };
            let minimal = shrink(&prog, &mut diverges);
            let path = save_found(
                &minimal,
                &UnicornOracle.run(&minimal.input()),
                &dontcare_flags(&minimal),
            );
            let d = compare(
                &UnicornOracle.run(&minimal.input()),
                &interp(&minimal),
                &dontcare_flags(&minimal),
            )
            .unwrap();
            panic!(
                "interpreter diverges from Unicorn (seed {seed}, saved {}):\n{:#?}\n{d}",
                path.display(),
                minimal.insns
            );
        }
    }
}

/// The interpreter must match the **real host CPU** (NativeOracle, task-186). Unlike
/// Unicorn, the native oracle decodes VEX/EVEX correctly, so it is the only automatic
/// check that the interpreter's BMI/AVX *semantics* (not just JIT-vs-interp codegen)
/// match hardware. x86-64/Linux only; inputs the host can't run natively (unsupported
/// instruction, etc.) return `None` and are skipped.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn native_matches_interp() {
    use x86jit_tests::fuzz::dontcare_flags;
    use x86jit_tests::native::run_native;

    let mut ran = 0u64;
    for seed in 1..300u64 {
        let prog = gen(seed, 12);
        let Some(native) = run_native(&prog.input()) else {
            continue; // host can't run this snippet natively — interp/Unicorn cover it
        };
        ran += 1;
        let interp_out = interp(&prog);
        if compare(&native, &interp_out, &dontcare_flags(&prog)).is_some() {
            let mut diverges = |p: &Prog| {
                run_native(&p.input())
                    .map(|n| compare(&n, &interp(p), &dontcare_flags(p)).is_some())
                    .unwrap_or(false)
            };
            let minimal = shrink(&prog, &mut diverges);
            // `run_native` has transient `None` modes (fork EAGAIN, alarm under load), so
            // the re-run can miss: fall back to the unshrunk program, then — if even that
            // won't reproduce — still save and panic with the seed rather than swallow the
            // divergence in an `unwrap`.
            let native_min = run_native(&minimal.input()).or_else(|| run_native(&prog.input()));
            let Some(native_min) = native_min else {
                let path = save_found(&minimal, &interp(&minimal), &dontcare_flags(&minimal));
                panic!(
                    "real-CPU divergence detected but not reproduced on re-run \
                     (seed {seed}, saved {}):\n{:#?}",
                    path.display(),
                    minimal.insns
                );
            };
            let path = save_found(&minimal, &native_min, &dontcare_flags(&minimal));
            let d = match compare(&native_min, &interp(&minimal), &dontcare_flags(&minimal)) {
                Some(d) => d,
                None => panic!(
                    "real-CPU divergence detected but not reproduced on re-run \
                     (seed {seed}, saved {}):\n{:#?}",
                    path.display(),
                    minimal.insns
                ),
            };
            panic!(
                "interpreter diverges from the real CPU (seed {seed}, saved {}):\n{:#?}\n{d}",
                path.display(),
                minimal.insns
            );
        }
    }
    assert!(
        ran > 0,
        "NativeOracle ran zero programs — the oracle is broken"
    );
}

/// Save a shrunk reproducer to `vectors/found/` as a permanent regression, with
/// the oracle's outcome baked in as the expectation. `dont_care` is the flag mask the
/// diverging leg used — bake it into the vector so corpus replay masks exactly the
/// flags the program leaves undefined, not just AF (machine-specific undefined flag
/// values otherwise fail replay forever).
fn save_found(prog: &Prog, oracle: &RunOutcome, dont_care: &[FlagName]) -> PathBuf {
    let input = prog.input();
    let mem_diff: Vec<MemChunk> = input
        .mem_init
        .iter()
        .zip(&oracle.mem)
        .filter(|(init, fin)| init.bytes != fin.bytes)
        .map(|(_, fin)| fin.clone())
        .collect();

    let name = format!("fuzz_{}", prog.seed);
    let vector = TestVector {
        name: name.clone(),
        note: format!("fuzzer divergence, seed {}", prog.seed),
        tags: vec!["found".into(), "fuzz".into()],
        cpu_init: input.cpu_init.clone(),
        mem_init: input.mem_init.clone(),
        entry: input.entry,
        run: input.run,
        expect: Expectation {
            cpu: oracle.cpu.clone(),
            mem_diff,
            exit: oracle.exit,
        },
        dont_care_flags: {
            // AF is always masked; keep it in the list even if the leg didn't name it.
            let mut flags = dont_care.to_vec();
            if !flags.contains(&FlagName::Af) {
                flags.push(FlagName::Af);
            }
            flags
        },
    };

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors/found");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.ron"));
    std::fs::write(&path, vector.to_ron()).unwrap();
    path
}
