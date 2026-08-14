//! x87 status-word exception flags, against the real CPU (task-328 AC#1).
//!
//! The six flags (IE DE ZE OE UE PE, bits 0-5 — SDM Vol 1 §8.1.3, Figure 8-4) were
//! storage that round-tripped through `fldenv`/`fnstenv` and nothing ever set them. A
//! guest that computed `1.0/0.0`, read the status word and branched on ZE took the wrong
//! branch, silently.
//!
//! Every case runs on the host CPU as well as the engine and compares the bits the
//! hardware produced. That matters more here than usual: the reporting rules are
//! conditional in ways that are easy to state backwards — masked underflow needs the
//! result to be *both* tiny and inexact (SDM Vol 1 §4.9.1.5), and ES (bit 7) is set only
//! by an **unmasked** flag, while the flag itself is set either way (§8.1.3.3). A test
//! that only asserted "ZE gets set" would pass over both mistakes.

//! **x86-64 Linux only.** Every case compares against the REAL CPU through
//! `native::run_native`, which forks a child and executes the snippet on the host — so on
//! any other host there is nothing to compare against and the module does not exist. The
//! architecture-asserted half lives in `x87_mf.rs`, which runs everywhere.
//!
//! Discovered by the FIRST execution of the aarch64 CI lane (2026-08-14): the import was
//! unconditional and the crate would not compile on ARM.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use iced_x86::code_asm::*;
use x86jit_core::InterpreterBackend;
use x86jit_tests::native::run_native;
use x86jit_tests::oracle::{run_with_backend, VectorInput};
use x86jit_tests::vector::{CpuSnapshot, MemChunk, MemKind, RunSpec};

const CODE: u64 = 0x21_0000;
const SCRATCH: u64 = 0x22_0000;

/// Operand slots inside the scratch page, 16 bytes apart so the 10-byte
/// double-extended forms do not overlap. They were 8 apart while only `f64` operands
/// existed, and the tbyte tests below then wrote B's exponent word over A's — which
/// showed up as an overflow that did not overflow and a spurious denormal flag.
const A: usize = 0;
const B: usize = 16;
const CW: usize = 64;
const SW: usize = 72;

const IE: u16 = 1 << 0;
const DE: u16 = 1 << 1;
const ZE: u16 = 1 << 2;
const OE: u16 = 1 << 3;
const UE: u16 = 1 << 4;
const PE: u16 = 1 << 5;
const ES: u16 = 1 << 7;
const B_BUSY: u16 = 1 << 15;

/// The bits this test compares: the six flags, the summary, and the 8087 busy mirror.
/// TOP and the condition codes are deliberately excluded — TOP is not an exception
/// report, and C0-C3 are `TASK-328` AC#4, not yet modelled.
const COMPARED: u16 = 0x3f | ES | B_BUSY;

/// `ST(1) op ST(0)` on two f64 operands under control word `cw`, returning the status
/// word both tiers stored, as `(native, ours)`.
fn flags_after(cw: u16, a: f64, b: f64, op: fn(&mut CodeAssembler)) -> (u16, u16) {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
    asm.fld(qword_ptr(SCRATCH + B as u64)).unwrap(); // -> ST(1) after the next push
    asm.fld(qword_ptr(SCRATCH + A as u64)).unwrap(); // -> ST(0)
    op(&mut asm);
    asm.fnstsw(word_ptr(SCRATCH + SW as u64)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    page[A..A + 8].copy_from_slice(&a.to_bits().to_le_bytes());
    page[B..B + 8].copy_from_slice(&b.to_bits().to_le_bytes());
    page[CW..CW + 2].copy_from_slice(&cw.to_le_bytes());

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

    let native = run_native(&input).expect("host runs the x87 op");
    let ours = run_with_backend(&input, Box::new(InterpreterBackend));
    (read_sw(&native), read_sw(&ours))
}

fn read_sw(out: &x86jit_tests::oracle::RunOutcome) -> u16 {
    let c = out.mem.iter().find(|c| c.addr == SCRATCH).unwrap();
    u16::from_le_bytes([c.bytes[SW], c.bytes[SW + 1]])
}

/// All six exceptions masked, round to nearest, 64-bit precision — the `finit` state.
const MASKED: u16 = 0x037F;

fn check(name: &str, cw: u16, a: f64, b: f64, op: fn(&mut CodeAssembler), expect: u16) {
    let (native, ours) = flags_after(cw, a, b, op);
    assert_eq!(
        native & COMPARED,
        expect,
        "{name}: the HOST disagrees with what this test expects — the expectation is \
         wrong, not the engine (native sw={native:#06x})"
    );
    assert_eq!(
        ours & COMPARED,
        native & COMPARED,
        "{name}: engine sw={ours:#06x} vs host sw={native:#06x}"
    );
}

fn divp(a: &mut CodeAssembler) {
    // ST(1) = ST(1) / ST(0), then pop — so ST(1) is the numerator.
    a.fdivp(st1, st0).unwrap();
}
fn mulp(a: &mut CodeAssembler) {
    a.fmulp(st1, st0).unwrap();
}
fn subp(a: &mut CodeAssembler) {
    a.fsubp(st1, st0).unwrap();
}

#[test]
fn divide_by_zero_sets_ze() {
    check("1.0 / 0.0", MASKED, 0.0, 1.0, divp, ZE);
}

#[test]
fn zero_over_zero_is_invalid_not_divide_by_zero() {
    // The distinction the flags exist for: 0/0 has no correctly-signed infinity to
    // return, so it is #IA and not #Z.
    check("0.0 / 0.0", MASKED, 0.0, 0.0, divp, IE);
}

#[test]
fn an_inexact_quotient_sets_pe() {
    check("1.0 / 3.0", MASKED, 3.0, 1.0, divp, PE);
}

#[test]
fn an_exact_quotient_sets_nothing() {
    check("4.0 / 2.0", MASKED, 2.0, 4.0, divp, 0);
}

/// Overflow and underflow need EIGHTY-bit operands, which is worth stating because the
/// obvious version of these two tests is wrong. `1e300 * 1e300` is `1e600` — enormous in
/// `f64` terms and utterly ordinary in double-extended, whose range reaches ~1.19e4932.
/// No pair of `f64` operands can overflow it through one multiply. The first version of
/// this test expected OE and the HOST said PE; the assertion is worded to place the blame
/// there, which is what made it obvious rather than a puzzle about the engine.
fn flags_after_f80(cw: u16, a: [u8; 10], b: [u8; 10], op: fn(&mut CodeAssembler)) -> (u16, u16) {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
    asm.fld(tbyte_ptr(SCRATCH + B as u64)).unwrap(); // -> ST(1) after the next push
    asm.fld(tbyte_ptr(SCRATCH + A as u64)).unwrap(); // -> ST(0)
    op(&mut asm);
    asm.fnstsw(word_ptr(SCRATCH + SW as u64)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    page[A..A + 10].copy_from_slice(&a);
    page[B..B + 10].copy_from_slice(&b);
    page[CW..CW + 2].copy_from_slice(&cw.to_le_bytes());

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
    let native = run_native(&input).expect("host runs the x87 op");
    let ours = run_with_backend(&input, Box::new(InterpreterBackend));
    (read_sw(&native), read_sw(&ours))
}

/// Double-extended encoding: significand (with its explicit integer bit) and the
/// sign+biased-exponent word.
fn f80(sig: u64, se: u16) -> [u8; 10] {
    let mut b = [0u8; 10];
    b[..8].copy_from_slice(&sig.to_le_bytes());
    b[8..].copy_from_slice(&se.to_le_bytes());
    b
}

/// Largest finite double-extended value: integer bit set, all fraction bits set, top
/// non-special exponent.
const MAX_FINITE: (u64, u16) = (u64::MAX, 0x7ffe);
/// Smallest normal: integer bit set, biased exponent 1.
const MIN_NORMAL: (u64, u16) = (0x8000_0000_0000_0000, 0x0001);

fn check_f80(
    name: &str,
    cw: u16,
    a: [u8; 10],
    b: [u8; 10],
    op: fn(&mut CodeAssembler),
    expect: u16,
) {
    let (native, ours) = flags_after_f80(cw, a, b, op);
    assert_eq!(
        native & COMPARED,
        expect,
        "{name}: the HOST disagrees with what this test expects — the expectation is \
         wrong, not the engine (native sw={native:#06x})"
    );
    assert_eq!(
        ours & COMPARED,
        native & COMPARED,
        "{name}: engine sw={ours:#06x} vs host sw={native:#06x}"
    );
}

#[test]
fn overflow_sets_oe_and_pe() {
    let m = f80(MAX_FINITE.0, MAX_FINITE.1);
    check_f80("max * max", MASKED, m, m, mulp, OE | PE);
}

#[test]
fn underflow_sets_ue_and_pe() {
    let t = f80(MIN_NORMAL.0, MIN_NORMAL.1);
    check_f80("min * min", MASKED, t, t, mulp, UE | PE);
}

#[test]
fn infinity_minus_infinity_is_invalid() {
    check("inf - inf", MASKED, f64::INFINITY, f64::INFINITY, subp, IE);
}

/// A masked exception sets its flag but NOT ES; unmasking it sets ES (and B, which
/// "reflects the contents of the ES flag"). This is the pair of assertions that a
/// single-case test would collapse.
#[test]
fn es_follows_the_mask_not_the_exception() {
    // ZM is bit 2 of the control word; clear it to unmask divide-by-zero.
    const UNMASK_ZE: u16 = MASKED & !(1 << 2);
    check("1.0/0.0 masked", MASKED, 0.0, 1.0, divp, ZE);
    check(
        "1.0/0.0 unmasked",
        UNMASK_ZE,
        0.0,
        1.0,
        divp,
        ZE | ES | B_BUSY,
    );
}

/// The masked response to overflow is NOT always infinity (SDM Vol 1 Table 4-11): three
/// of the four rounding modes return the largest finite value in the direction that
/// leans away from infinity.
///
/// This exists because fixing that changed nothing in 831 tests — the engine returned
/// `inf` for every mode and no test looked. The value is compared against the host, not
/// against a constant written here, so the table is being checked rather than restated.
#[test]
fn masked_overflow_follows_the_rounding_mode() {
    const RC: [(u16, &str); 4] = [(0, "nearest"), (1, "down"), (2, "up"), (3, "zero")];
    let m = f80(MAX_FINITE.0, MAX_FINITE.1);
    let neg = f80(MAX_FINITE.0, MAX_FINITE.1 | 0x8000);

    let mut seen = std::collections::BTreeSet::new();
    for (rc, name) in RC {
        // Positive and negative true results: Table 4-11 is indexed by both.
        for (b, sign) in [(m, "+"), (neg, "-")] {
            let cw = 0x037F | (rc << 10);
            let (native, ours) = product_bytes(cw, m, b);
            println!("rc={name:<8} {sign} -> {}", hex::encode(native));
            assert_eq!(
                ours,
                native,
                "rc={name} {sign}: engine {} vs host {}",
                hex::encode(ours),
                hex::encode(native)
            );
            seen.insert(native);
        }
    }
    // Infinity for some modes, largest finite for others — if the engine collapsed them
    // all to infinity again there would be only two distinct results (±inf).
    assert!(
        seen.len() > 2,
        "every rounding mode produced the same two values; Table 4-11 is not being \
         applied ({} distinct)",
        seen.len()
    );
}

/// `ST(1) * ST(0)` stored as a tbyte, from both tiers.
fn product_bytes(cw: u16, a: [u8; 10], b: [u8; 10]) -> ([u8; 10], [u8; 10]) {
    const OUT: usize = 96;
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
    asm.fld(tbyte_ptr(SCRATCH + B as u64)).unwrap();
    asm.fld(tbyte_ptr(SCRATCH + A as u64)).unwrap();
    asm.fmulp(st1, st0).unwrap();
    asm.fstp(tbyte_ptr(SCRATCH + OUT as u64)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    page[A..A + 10].copy_from_slice(&a);
    page[B..B + 10].copy_from_slice(&b);
    page[CW..CW + 2].copy_from_slice(&cw.to_le_bytes());

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
    let native = run_native(&input).expect("host runs the x87 op");
    let ours = run_with_backend(&input, Box::new(InterpreterBackend));
    let grab = |o: &x86jit_tests::oracle::RunOutcome| {
        let c = o.mem.iter().find(|c| c.addr == SCRATCH).unwrap();
        let mut r = [0u8; 10];
        r.copy_from_slice(&c.bytes[OUT..OUT + 10]);
        r
    };
    (grab(&native), grab(&ours))
}

/// Stack overflow (#IS): a ninth push onto an eight-deep stack (task-328 AC#2).
///
/// "An instruction attempts to load a non-empty x87 FPU register" — non-empty being any
/// tag other than 11 (SDM Vol 1 §8.5.1.1). It sets IE and SF, and C1 to **1**; underflow
/// sets the same two flags with C1 **0**, so C1 is the only thing that tells them apart
/// and a test that ignored it would not be testing the distinction at all.
///
/// Host-witnessed, including C1 — which is why SF and C1 are added to the compared bits
/// here rather than trusted from the manual.
#[test]
fn a_ninth_push_is_a_stack_overflow() {
    const SF: u16 = 1 << 6;
    const C1: u16 = 1 << 9;
    const COMPARED_IS: u16 = 0x3f | SF | ES | C1 | B_BUSY;

    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
    for _ in 0..9 {
        asm.fld1().unwrap(); // the ninth wraps onto a register that already has a value
    }
    asm.fnstsw(word_ptr(SCRATCH + SW as u64)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    page[CW..CW + 2].copy_from_slice(&MASKED.to_le_bytes());
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
    let native = run_native(&input).expect("host runs nine pushes");
    let ours = run_with_backend(&input, Box::new(InterpreterBackend));
    let (n, o) = (read_sw(&native), read_sw(&ours));

    assert_eq!(
        n & COMPARED_IS,
        IE | SF | C1,
        "the HOST disagrees with what this test expects (native sw={n:#06x})"
    );
    assert_eq!(
        o & COMPARED_IS,
        n & COMPARED_IS,
        "engine sw={o:#06x} vs host sw={n:#06x}"
    );
}

/// Stack underflow (#IS): referencing an EMPTY register as a source (task-328 AC#2).
///
/// Two shapes, and the second is the point. `fdivp` on an empty stack pops, so a check
/// placed in `pop()` would catch it. `fadd st(0), st(3)` with ST(3) empty READS without
/// popping, and a `pop()`-based check reports nothing — half a rule presented as a whole
/// one. The detection point is the read, which is why both are here.
///
/// C1 must be **0**: overflow and underflow set the same IE and SF, and C1 is the only
/// bit that separates them, so it is in the compared set.
fn underflow_flags(build: fn(&mut CodeAssembler)) -> (u16, u16) {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
    build(&mut asm);
    asm.fnstsw(word_ptr(SCRATCH + SW as u64)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    page[CW..CW + 2].copy_from_slice(&MASKED.to_le_bytes());
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
    let native = run_native(&input).expect("host runs the x87 op");
    let ours = run_with_backend(&input, Box::new(InterpreterBackend));
    (read_sw(&native), read_sw(&ours))
}

const SF_BIT: u16 = 1 << 6;
const C1_BIT: u16 = 1 << 9;
const COMPARED_IS: u16 = 0x3f | SF_BIT | ES | C1_BIT | B_BUSY;

#[test]
fn popping_an_empty_stack_is_an_underflow() {
    // `finit` leaves every register empty, so ST(0) is empty here.
    let (n, o) = underflow_flags(|a| {
        a.fstp(qword_ptr(SCRATCH + 128)).unwrap();
    });
    // IE and SF with C1 CLEAR — the same two flags an overflow raises, and C1 is the
    // only bit that says which happened.
    assert_eq!(
        n & COMPARED_IS,
        IE | SF_BIT,
        "the HOST disagrees with what this test expects (native sw={n:#06x})"
    );
    assert_eq!(
        o & COMPARED_IS,
        n & COMPARED_IS,
        "engine {o:#06x} vs host {n:#06x}"
    );
}

/// `fstp tbyte` on an empty stack, which the 64-bit form above cannot stand in for.
///
/// Found by review. `FstpF80` reads the register's raw bytes directly — task-324 made it
/// a pure move so a pseudo-denormal or unnormal survives the round trip verbatim — which
/// left it with no `st()` call for the underflow migration to catch. Every sibling store
/// goes through `operand!`; this one did not, so it wrote ten bytes of stale register
/// data and popped, silently. SDM Vol 1 §8.5.1.1 names this case explicitly: "including
/// attempting to write the contents of an empty register to memory".
#[test]
fn storing_an_empty_register_as_tbyte_is_an_underflow() {
    let (n, o) = underflow_flags(|a| {
        a.fstp(tbyte_ptr(SCRATCH + 128)).unwrap();
    });
    assert_eq!(
        n & COMPARED_IS,
        IE | SF_BIT,
        "the HOST disagrees with what this test expects (native sw={n:#06x})"
    );
    assert_eq!(
        o & COMPARED_IS,
        n & COMPARED_IS,
        "engine {o:#06x} vs host {n:#06x}"
    );
}

#[test]
fn reading_an_empty_register_without_popping_is_an_underflow() {
    // One push, so ST(0) is valid and ST(3) is empty. `fld st(3)` reads ST(3) as a
    // source and PUSHES — the opposite of a pop, so a check placed in `pop()` cannot
    // see it at all.
    let (n, o) = underflow_flags(|a| {
        a.fld1().unwrap();
        a.fld(st3).unwrap();
    });
    assert_eq!(
        n & COMPARED_IS,
        IE | SF_BIT,
        "the HOST disagrees with what this test expects (native sw={n:#06x})"
    );
    assert_eq!(
        o & COMPARED_IS,
        n & COMPARED_IS,
        "engine {o:#06x} vs host {n:#06x}"
    );
}

/// `ficom` / `ficomp` (task-328 AC#4), against the host.
///
/// These stayed unlifted for as long as the condition codes did not exist, because they
/// report through C0/C2/C3 rather than EFLAGS. SDM Vol 2A Table 3-28:
///
/// | condition   | C3 | C2 | C0 |
/// |-------------|----|----|----|
/// | ST(0) > SRC |  0 |  0 |  0 |
/// | ST(0) < SRC |  0 |  0 |  1 |
/// | ST(0) = SRC |  1 |  0 |  0 |
/// | Unordered   |  1 |  1 |  1 |
///
/// The NaN case matters twice over: it is the one that separates `ficom` from `fucom`
/// (an ordered compare raises #IA on a *quiet* NaN too — "#IA: One or both operands are
/// NaN values or have unsupported formats"), and it is the only way C2 is ever set here.
#[test]
fn ficom_sets_the_condition_codes() {
    const C0: u16 = 1 << 8;
    const C1: u16 = 1 << 9;
    const C2: u16 = 1 << 10;
    const C3: u16 = 1 << 14;
    const CODES: u16 = C0 | C1 | C2 | C3 | IE;

    // (ST(0) value as f64, integer source, expected code bits)
    let cases: [(f64, i32, u16); 4] = [
        (5.0, 3, 0),                      // greater
        (1.0, 3, C0),                     // less
        (3.0, 3, C3),                     // equal
        (f64::NAN, 3, C3 | C2 | C0 | IE), // unordered, and #IA
    ];

    for (top, src, expect) in cases {
        let mut asm = CodeAssembler::new(64).unwrap();
        asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
        asm.fld(qword_ptr(SCRATCH + A as u64)).unwrap();
        asm.ficom(dword_ptr(SCRATCH + B as u64)).unwrap();
        asm.fnstsw(word_ptr(SCRATCH + SW as u64)).unwrap();
        asm.hlt().unwrap();
        let code = asm.assemble(CODE).unwrap();

        let mut page = vec![0u8; 0x1000];
        page[A..A + 8].copy_from_slice(&top.to_bits().to_le_bytes());
        page[B..B + 4].copy_from_slice(&src.to_le_bytes());
        page[CW..CW + 2].copy_from_slice(&MASKED.to_le_bytes());
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
        let native = run_native(&input).expect("host runs ficom");
        let ours = run_with_backend(&input, Box::new(InterpreterBackend));
        let (n, o) = (read_sw(&native), read_sw(&ours));
        assert_eq!(
            n & CODES,
            expect,
            "ST(0)={top} vs {src}: the HOST disagrees with what this test expects \
             (native sw={n:#06x})"
        );
        assert_eq!(
            o & CODES,
            n & CODES,
            "ST(0)={top} vs {src}: engine sw={o:#06x} vs host sw={n:#06x}"
        );
    }
}

/// The denormal-operand exception (#D), against the host.
///
/// "The processor reports the denormal-operand exception if an ARITHMETIC instruction
/// attempts to operate on a denormal operand" (SDM Vol 1 §4.9.1.2) — arithmetic, which is
/// why it is raised at the arithmetic sites and not inside the shared operand read that
/// `fld`, `fst` and the compares also go through.
///
/// It needs the RAW bytes: `F80::from_bytes` folds a denormal into the normal class with
/// a lower exponent, so by the time the value reaches arithmetic there is nothing left to
/// look at. That is the whole reason DE was left out when the other five landed.
#[test]
fn a_denormal_operand_sets_de() {
    // Smallest positive f64 denormal: exponent zero, significand 1.
    let denorm = f64::from_bits(1);
    assert!(
        denorm.is_subnormal(),
        "the test operand must actually be denormal"
    );

    // A denormal times one: DE, and the product is exact so nothing else fires except
    // what the host says.
    let (n, o) = flags_after(MASKED, 1.0, denorm, mulp);
    assert!(
        n & DE != 0,
        "the HOST did not report DE for a denormal operand (native sw={n:#06x}) — the \
         expectation is wrong, not the engine"
    );
    assert_eq!(
        o & COMPARED,
        n & COMPARED,
        "engine sw={o:#06x} vs host sw={n:#06x}"
    );

    // Two normal operands must NOT set it, or "raise DE always" would pass above.
    let (n2, o2) = flags_after(MASKED, 1.0, 3.0, mulp);
    assert_eq!(
        n2 & DE,
        0,
        "the host set DE for two normal operands (sw={n2:#06x})"
    );
    assert_eq!(
        o2 & COMPARED,
        n2 & COMPARED,
        "engine sw={o2:#06x} vs host sw={n2:#06x}"
    );
}

/// The other two paths that can meet a denormal, both arbitrated by the host rather than
/// by my reading of the manual — which was wrong once already for `fld`.
///
/// - `fadd qword [denormal]`: the memory operand of an ARITHMETIC instruction.
/// - an 80-bit denormal already sitting in a register, used as an arithmetic operand.
///   This one cannot arrive through `fld m32/m64` — those convert, and the result is an
///   ordinary 80-bit value — so it takes `fld tbyte`, which is a pure move.
#[test]
fn denormal_operands_of_arithmetic_match_the_host() {
    // (a) memory operand.
    let denorm = f64::from_bits(1);
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
    asm.fld1().unwrap();
    asm.fadd(qword_ptr(SCRATCH + A as u64)).unwrap();
    asm.fnstsw(word_ptr(SCRATCH + SW as u64)).unwrap();
    asm.hlt().unwrap();
    let (n, o) = run_snippet(asm, |page| {
        page[A..A + 8].copy_from_slice(&denorm.to_bits().to_le_bytes());
    });
    assert_ne!(
        n & DE,
        0,
        "host did not set DE for a denormal memory operand ({n:#06x})"
    );
    assert_eq!(
        o & COMPARED,
        n & COMPARED,
        "engine {o:#06x} vs host {n:#06x}"
    );

    // (b) an 80-bit denormal in a register: biased exponent 0, significand non-zero.
    let d80 = f80(1, 0x0000);
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(SCRATCH + CW as u64)).unwrap();
    asm.fld(tbyte_ptr(SCRATCH + A as u64)).unwrap(); // a move, so it stays denormal
    asm.fld1().unwrap();
    asm.fmulp(st1, st0).unwrap();
    asm.fnstsw(word_ptr(SCRATCH + SW as u64)).unwrap();
    asm.hlt().unwrap();
    let (n, o) = run_snippet(asm, |page| {
        page[A..A + 10].copy_from_slice(&d80);
    });
    assert_ne!(
        n & DE,
        0,
        "host did not set DE for a denormal register operand ({n:#06x})"
    );
    assert_eq!(
        o & COMPARED,
        n & COMPARED,
        "engine {o:#06x} vs host {n:#06x}"
    );
}

/// Assemble, run on both tiers, return their status words.
fn run_snippet(asm: CodeAssembler, fill: impl FnOnce(&mut Vec<u8>)) -> (u16, u16) {
    let mut asm = asm;
    let code = asm.assemble(CODE).unwrap();
    let mut page = vec![0u8; 0x1000];
    page[CW..CW + 2].copy_from_slice(&MASKED.to_le_bytes());
    fill(&mut page);
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
    let native = run_native(&input).expect("host runs the snippet");
    let ours = run_with_backend(&input, Box::new(InterpreterBackend));
    (read_sw(&native), read_sw(&ours))
}
