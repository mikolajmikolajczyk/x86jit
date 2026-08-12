//! task-324 AC#1: the x87 control word governs arithmetic, not just integer conversion.
//!
//! Rounding control (bits 11:10) and precision control (bits 9:8) each change the result
//! of an ordinary `fdiv`/`fadd`. Before this, `rc` reached only `fist`/`fistp`, and every
//! add/sub/mul/div called `F80` fixed at nearest-even with a 64-bit significand — so a
//! guest that ran `fldcw` to select round-toward-zero or 24-bit precision, which is the
//! entire purpose of the instruction, got the default behaviour and no trap.
//!
//! Every assertion is against the real CPU. Two of them are structural: the twelve
//! (RC, PC) combinations must not all produce the same bytes, and each field must move
//! the result on its own. Without those a test that merely compared engine to engine, or
//! that happened to pick an exactly-representable operand, would pass against an engine
//! that ignores the control word entirely.

use std::collections::BTreeSet;

use iced_x86::code_asm::*;
use x86jit_core::InterpreterBackend;
use x86jit_tests::compare::compare;
use x86jit_tests::native::run_native;
use x86jit_tests::oracle::{run_with_backend, VectorInput};
use x86jit_tests::vector::{CpuSnapshot, MemChunk, MemKind, RunSpec};

const CODE: u64 = 0x21_0000;
const SCRATCH: u64 = 0x22_0000;

/// RC field values (SDM Vol 1 §4.8.4.1, Table 4-9).
const RC: [(u16, &str); 4] = [
    (0, "nearest-even"),
    (1, "toward -inf"),
    (2, "toward +inf"),
    (3, "toward zero"),
];
/// PC field values (SDM Vol 1 §8.1.5.2, Table 8-2); `01` is reserved and not tested.
const PC: [(u16, &str); 3] = [(0, "24-bit"), (2, "53-bit"), (3, "64-bit")];

/// `fldcw` the given control word, divide `a` by `b`, store the 80-bit result.
/// Returns the ten stored bytes, having first asserted interp == the real CPU.
fn divide_under(cw: u16, a: f64, b: f64) -> [u8; 10] {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + 64)).unwrap();
    asm.fld(qword_ptr(SCRATCH + 8)).unwrap(); // divisor -> ST(1) after the next push
    asm.fld(qword_ptr(SCRATCH)).unwrap(); // dividend -> ST(0)
    asm.fdivp(st1, st0).unwrap(); // ST(1) = ST(1) / ST(0)... see note below
    asm.fstp(tbyte_ptr(SCRATCH + 32)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    // ST(1) is the divisor and ST(0) the dividend, and `fdivp st1, st0` computes
    // ST(1)/ST(0) — so put the numerator second.
    page[..8].copy_from_slice(&b.to_le_bytes());
    page[8..16].copy_from_slice(&a.to_le_bytes());
    page[64..66].copy_from_slice(&cw.to_le_bytes());

    let input = VectorInput {
        cpu_init: CpuSnapshot::default(),
        mem_init: vec![
            MemChunk {
                addr: CODE,
                bytes: code,
                kind: MemKind::Ram,
            },
            MemChunk {
                addr: SCRATCH,
                bytes: page,
                kind: MemKind::Ram,
            },
        ],
        entry: CODE,
        run: RunSpec::UntilExit,
    };
    let native = run_native(&input).expect("host runs an x87 divide");
    let interp = run_with_backend(&input, Box::new(InterpreterBackend));
    assert!(
        compare(&native, &interp, &[]).is_none(),
        "interp diverges from the real CPU at cw={cw:#06x}:\n{:#?}",
        compare(&native, &interp, &[])
    );
    let c = native.mem.iter().find(|c| c.addr == SCRATCH).unwrap();
    let mut r = [0u8; 10];
    r.copy_from_slice(&c.bytes[32..42]);
    r
}

/// Base control word: all exceptions masked, RC and PC cleared so the sweep sets them.
fn cw(rc: u16, pc: u16) -> u16 {
    0x003F | (pc << 8) | (rc << 10)
}

#[test]
fn rounding_and_precision_control_reach_arithmetic() {
    // 1/3 is inexact at every precision, so every field can show.
    let mut seen: BTreeSet<[u8; 10]> = BTreeSet::new();
    for (rc, rc_name) in RC {
        for (pc, pc_name) in PC {
            let r = divide_under(cw(rc, pc), 1.0, 3.0);
            println!("rc={rc_name:<12} pc={pc_name}  -> {}", hex::encode(r));
            seen.insert(r);
        }
    }
    // Six, not twelve: the quotient is positive, so toward-zero collapses onto
    // toward-−∞ and (for this operand) nearest onto toward-+∞, leaving two directions ×
    // three precisions. An engine that ignores the control word gives ONE answer, which
    // is what this number is here to exclude.
    assert_eq!(
        seen.len(),
        6,
        "expected two rounding directions × three precisions; got {} distinct results \
         across the 12 (RC, PC) settings",
        seen.len()
    );
}

/// Rounding control alone, at full precision: the four modes of an inexact quotient are
/// three distinct values (nearest and one directed mode must agree — which one depends on
/// the operand, so this asserts the count, not which).
#[test]
fn rounding_control_alone_changes_the_result() {
    let results: Vec<[u8; 10]> = RC
        .iter()
        .map(|&(rc, _)| divide_under(cw(rc, 3), 1.0, 3.0))
        .collect();
    let distinct: BTreeSet<_> = results.iter().collect();
    assert!(
        distinct.len() >= 2,
        "rounding control does not change an inexact quotient"
    );
    // Toward zero and toward -inf agree for a positive result; toward +inf must not.
    assert_eq!(
        results[3], results[1],
        "for a positive quotient, toward-zero and toward-minus-infinity agree"
    );
    assert_ne!(
        results[2], results[3],
        "toward +inf must round the other way"
    );
}

/// Precision control alone, at nearest-even: "when reduced precision is specified, the
/// rounding of the significand value clears the unused bits on the right to zeros"
/// (SDM Vol 1 §8.1.5.2). So 24-bit and 53-bit results carry trailing zero bits that the
/// 64-bit one does not.
#[test]
fn precision_control_narrows_the_significand() {
    let at = |pc| divide_under(cw(0, pc), 1.0, 3.0);
    let sig = |b: [u8; 10]| u64::from_le_bytes(b[..8].try_into().unwrap());

    let (s24, s53, s64) = (sig(at(0)), sig(at(2)), sig(at(3)));
    assert_eq!(
        s24 & ((1u64 << 40) - 1),
        0,
        "24-bit leaves the low 40 bits clear"
    );
    assert_eq!(
        s53 & ((1u64 << 11) - 1),
        0,
        "53-bit leaves the low 11 bits clear"
    );
    assert_ne!(s64 & ((1u64 << 11) - 1), 0, "64-bit uses them");
    assert_ne!(s24, s53);
    assert_ne!(s53, s64);
}

/// The same for addition, so the control word is threaded through the shared rounding
/// rather than pasted into the divide path.
#[test]
fn addition_honours_the_control_word_too() {
    // 1.0 + 2^-70: inexact at every precision, and the direction of the round decides
    // whether the low bit moves at all.
    let tiny = f64::from_bits(0x3B90_0000_0000_0000); // 2^-70
    let up = divide_addition(cw(2, 3), 1.0, tiny);
    let down = divide_addition(cw(1, 3), 1.0, tiny);
    let zero = divide_addition(cw(3, 3), 1.0, tiny);
    assert_ne!(
        up, down,
        "toward +inf and toward -inf must differ on an inexact sum"
    );
    assert_eq!(
        down, zero,
        "for a positive sum, toward -inf and toward zero agree"
    );
}

/// `fadd` counterpart of [`divide_under`].
fn divide_addition(cw: u16, a: f64, b: f64) -> [u8; 10] {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + 64)).unwrap();
    asm.fld(qword_ptr(SCRATCH + 8)).unwrap();
    asm.fld(qword_ptr(SCRATCH)).unwrap();
    asm.faddp(st1, st0).unwrap();
    asm.fstp(tbyte_ptr(SCRATCH + 32)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    page[..8].copy_from_slice(&b.to_le_bytes());
    page[8..16].copy_from_slice(&a.to_le_bytes());
    page[64..66].copy_from_slice(&cw.to_le_bytes());
    let input = VectorInput {
        cpu_init: CpuSnapshot::default(),
        mem_init: vec![
            MemChunk {
                addr: CODE,
                bytes: code,
                kind: MemKind::Ram,
            },
            MemChunk {
                addr: SCRATCH,
                bytes: page,
                kind: MemKind::Ram,
            },
        ],
        entry: CODE,
        run: RunSpec::UntilExit,
    };
    let native = run_native(&input).expect("host runs an x87 add");
    let interp = run_with_backend(&input, Box::new(InterpreterBackend));
    assert!(
        compare(&native, &interp, &[]).is_none(),
        "interp diverges from the real CPU at cw={cw:#06x}:\n{:#?}",
        compare(&native, &interp, &[])
    );
    let c = native.mem.iter().find(|c| c.addr == SCRATCH).unwrap();
    let mut r = [0u8; 10];
    r.copy_from_slice(&c.bytes[32..42]);
    r
}
