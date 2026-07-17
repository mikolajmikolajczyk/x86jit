//! Empirical audit (ONE-OFF probe, task ad-hoc): does the interpreter wrongly ZERO
//! the ymm upper half (bits 255:128) on a legacy (non-VEX) SSE instruction?
//!
//! A legacy SSE op like `paddb xmm0, xmm1` writes only bits 127:0 of the destination
//! XMM and MUST PRESERVE bits 255:128 (the ymm upper). Only VEX/EVEX encodings zero
//! the upper. For each of the 62 legacy-SSE ops below we assemble a single-instruction
//! program (`op xmm0, xmm1`), seed `ymm_hi[0]` with a nonzero sentinel, run it through
//! BOTH the interpreter backend AND the NativeOracle (the real host CPU = ground truth),
//! and compare the destination's upper half.
//!
//! Run:
//!   cargo test --release -p x86jit-tests --test legacy_upper_audit -- --ignored --nocapture

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use iced_x86::code_asm::*;
use x86jit_tests::native::run_native;
use x86jit_tests::oracle::{run_with_backend, VectorInput};
use x86jit_tests::vector::{CpuSnapshot, MemChunk, MemKind, RunSpec};

const CODE: u64 = 0x21_0000;

/// Nonzero full-width sentinel placed in the destination's ymm upper half.
const SENTINEL: u128 = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;

type Emit = Box<dyn Fn(&mut CodeAssembler)>;

/// The 62 legacy-SSE ops. Each closure emits exactly `op xmm0, xmm1` (round* carry an
/// imm8 rounding mode). Three ops (punpcklqdq/packssdw/packsswb) appear twice — once in
/// the `vbin` table, once in the `vnew` table — kept as distinct rows (suffix `#vnew`)
/// so the row count matches the 62 in the task.
fn ops() -> Vec<(&'static str, Emit)> {
    macro_rules! op {
        ($name:literal, $m:ident) => {
            (
                $name,
                Box::new(|a: &mut CodeAssembler| {
                    a.$m(xmm0, xmm1).unwrap();
                }) as Emit,
            )
        };
    }
    macro_rules! rnd {
        ($name:literal, $m:ident, $imm:expr) => {
            (
                $name,
                Box::new(|a: &mut CodeAssembler| {
                    a.$m(xmm0, xmm1, $imm as u32).unwrap();
                }) as Emit,
            )
        };
    }
    vec![
        // --- vbin (42) ---
        op!("paddb", paddb),
        op!("paddw", paddw),
        op!("paddd", paddd),
        op!("paddq", paddq),
        op!("psubb", psubb),
        op!("psubw", psubw),
        op!("psubd", psubd),
        op!("psubq", psubq),
        op!("pand", pand),
        op!("por", por),
        op!("pxor", pxor),
        op!("pandn", pandn),
        op!("pcmpeqb", pcmpeqb),
        op!("pcmpeqw", pcmpeqw),
        op!("pcmpeqd", pcmpeqd),
        op!("pcmpgtb", pcmpgtb),
        op!("pcmpgtw", pcmpgtw),
        op!("pcmpgtd", pcmpgtd),
        op!("punpcklbw", punpcklbw),
        op!("punpcklwd", punpcklwd),
        op!("punpckldq", punpckldq),
        op!("punpcklqdq", punpcklqdq),
        op!("punpckhbw", punpckhbw),
        op!("punpckhwd", punpckhwd),
        op!("punpckhdq", punpckhdq),
        op!("punpckhqdq", punpckhqdq),
        op!("packuswb", packuswb),
        op!("pminub", pminub),
        op!("pmaxub", pmaxub),
        op!("paddsb", paddsb),
        op!("paddsw", paddsw),
        op!("paddusb", paddusb),
        op!("paddusw", paddusw),
        op!("psubsb", psubsb),
        op!("psubsw", psubsw),
        op!("psubusb", psubusb),
        op!("psubusw", psubusw),
        op!("pavgb", pavgb),
        op!("pavgw", pavgw),
        op!("packsswb", packsswb),
        op!("packssdw", packssdw),
        op!("pmaddwd", pmaddwd),
        // --- vnew (20) ---
        rnd!("roundps", roundps, 0), // nearest
        rnd!("roundpd", roundpd, 1), // floor
        rnd!("roundss", roundss, 2), // ceil
        rnd!("roundsd", roundsd, 3), // trunc
        op!("haddps", haddps),
        op!("haddpd", haddpd),
        op!("hsubps", hsubps),
        op!("hsubpd", hsubpd),
        op!("addsubps", addsubps),
        op!("addsubpd", addsubpd),
        op!("phaddw", phaddw),
        op!("phaddd", phaddd),
        op!("phaddsw", phaddsw),
        op!("phsubw", phsubw),
        op!("phsubd", phsubd),
        op!("phsubsw", phsubsw),
        op!("psadbw", psadbw),
        op!("punpcklqdq#vnew", punpcklqdq),
        op!("packssdw#vnew", packssdw),
        op!("packsswb#vnew", packsswb),
    ]
}

/// A fresh init snapshot: dest (reg 0) upper = SENTINEL, source (reg 1) upper nonzero,
/// and xmm[0]/xmm[1] carry nonzero data so the compute actually runs on real operands.
fn fresh_init() -> CpuSnapshot {
    let mut init = CpuSnapshot {
        rip: CODE,
        ..Default::default()
    };
    init.xmm[0] = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;
    init.xmm[1] = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00;
    init.ymm_hi[0] = SENTINEL;
    init.ymm_hi[1] = 0xCAFE_F00D_CAFE_F00D_CAFE_F00D_CAFE_F00D;
    init
}

fn build_input(emit: &Emit) -> VectorInput {
    let mut a = CodeAssembler::new(64).unwrap();
    emit(&mut a);
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();
    VectorInput {
        cpu_init: fresh_init(),
        mem_init: vec![MemChunk {
            addr: CODE,
            bytes: code,
            kind: MemKind::Ram,
        }],
        entry: CODE,
        run: RunSpec::UntilExit,
    }
}

#[test]
#[ignore = "empirical audit probe; run with --ignored --nocapture"]
fn legacy_sse_upper_half_audit() {
    assert!(
        std::is_x86_feature_detected!("avx"),
        "host lacks AVX — cannot load/capture the ymm upper half natively"
    );

    let pv = |v: u128| if v == SENTINEL { "y" } else { "n" };

    println!();
    println!(
        "{:<18} | {:<34} | {:<34} | VERDICT",
        "op_name", "native_ymm_hi[0] (preserved?)", "interp_ymm_hi[0] (preserved?)"
    );
    println!("{}", "-".repeat(110));

    let (mut ok, mut bug, mut anomaly, mut unavail) = (0u32, 0u32, 0u32, 0u32);
    let mut bug_ops: Vec<&str> = Vec::new();
    let mut anomaly_ops: Vec<&str> = Vec::new();

    for (name, emit) in ops() {
        let input = build_input(&emit);

        let interp = run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        let interp_hi = interp.cpu.ymm_hi[0];

        let native = run_native(&input);

        let (native_cell, verdict) = match native {
            None => {
                unavail += 1;
                (
                    "UNAVAILABLE (None)".to_string(),
                    "native-unavailable".to_string(),
                )
            }
            Some(out) => {
                let nhi = out.cpu.ymm_hi[0];
                let native_preserved = nhi == SENTINEL;
                let interp_preserved = interp_hi == SENTINEL;
                let cell = format!("{nhi:032x} ({})", pv(nhi));
                let verdict = if !native_preserved {
                    anomaly += 1;
                    anomaly_ops.push(name);
                    "native-anomaly".to_string()
                } else if interp_preserved {
                    ok += 1;
                    "OK".to_string()
                } else {
                    bug += 1;
                    bug_ops.push(name);
                    "BUG-clears-upper".to_string()
                };
                (cell, verdict)
            }
        };

        println!(
            "{:<18} | {:<34} | {:<34} | {}",
            name,
            native_cell,
            format!("{interp_hi:032x} ({})", pv(interp_hi)),
            verdict
        );
    }

    println!("{}", "-".repeat(110));
    println!(
        "TOTALS: {} ops | OK={ok} | BUG-clears-upper={bug} | native-anomaly={anomaly} | native-unavailable={unavail}",
        ok + bug + anomaly + unavail
    );
    println!("sentinel = {SENTINEL:032x}");
    if !bug_ops.is_empty() {
        println!(
            "\nBUG ops (native preserved, interp cleared) [{}]:",
            bug_ops.len()
        );
        for o in &bug_ops {
            println!("  - {o}");
        }
    } else {
        println!("\nNo BUG ops found.");
    }
    if !anomaly_ops.is_empty() {
        println!(
            "\nNATIVE ANOMALIES (hardware did NOT preserve — premise wrong for these) [{}]:",
            anomaly_ops.len()
        );
        for o in &anomaly_ops {
            println!("  - {o}");
        }
    }
}
