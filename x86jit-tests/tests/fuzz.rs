//! Differential fuzzing (M4, testing.md §7). Random valid programs, two engines,
//! any state divergence is a bug — shrunk to a minimal reproducer, seed recorded,
//! and auto-saved to `vectors/found/` before the test fails.
//!
//! - `jit_matches_interp` (default): JIT vs interpreter, exact match required —
//!   the JIT mirrors the interpreter, so any divergence is a codegen bug.
//! - `unicorn_matches_interp` (`--features unicorn`): the lift/interp vs the
//!   Unicorn truth, with undefined AF masked.

use std::path::PathBuf;

use x86jit_core::InterpreterBackend;
use x86jit_cranelift::JitBackend;
use x86jit_tests::compare::compare;
use x86jit_tests::fuzz::{gen, shrink, Prog};
use x86jit_tests::oracle::{run_with_backend, RunOutcome};
use x86jit_tests::vector::{Expectation, FlagName, MemChunk, TestVector};

fn interp(prog: &Prog) -> RunOutcome {
    run_with_backend(&prog.input(), Box::new(InterpreterBackend))
}

fn jit(prog: &Prog) -> RunOutcome {
    run_with_backend(&prog.input(), Box::new(JitBackend::new()))
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
            let path = save_found(&minimal, &interp(&minimal));
            let d = compare(&interp(&minimal), &jit(&minimal), &[]).unwrap();
            panic!(
                "JIT diverges from interpreter (seed {seed}, saved {}):\n{:#?}\n{d}",
                path.display(),
                minimal.insns
            );
        }
    }
}

#[cfg(feature = "unicorn")]
#[test]
fn unicorn_matches_interp() {
    use x86jit_tests::oracle::Oracle;
    use x86jit_tests::unicorn::UnicornOracle;

    // AF is architecturally undefined after logic ops — mask it.
    let mask = [FlagName::Af];
    for seed in 1..300u64 {
        let prog = gen(seed, 12);
        let interp_out = interp(&prog);
        let uni = UnicornOracle.run(&prog.input());
        if compare(&uni, &interp_out, &mask).is_some() {
            let mut diverges =
                |p: &Prog| compare(&UnicornOracle.run(&p.input()), &interp(p), &mask).is_some();
            let minimal = shrink(&prog, &mut diverges);
            let path = save_found(&minimal, &UnicornOracle.run(&minimal.input()));
            let d = compare(
                &UnicornOracle.run(&minimal.input()),
                &interp(&minimal),
                &mask,
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

/// Save a shrunk reproducer to `vectors/found/` as a permanent regression, with
/// the oracle's outcome baked in as the expectation.
fn save_found(prog: &Prog, oracle: &RunOutcome) -> PathBuf {
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
        dont_care_flags: vec![FlagName::Af],
    };

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors/found");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.ron"));
    std::fs::write(&path, vector.to_ron()).unwrap();
    path
}
