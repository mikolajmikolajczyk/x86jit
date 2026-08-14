//! task-324: the double-extended encodings the x87 treats specially, against the real CPU.
//!
//! Every case here is a raw 10-byte operand loaded with `fld tbyte`, combined with a
//! known value, and stored back with `fstp tbyte` — so the assertion is on bytes the
//! hardware produced, not on a classification the engine agrees with itself about.
//!
//! Why the native oracle and not Unicorn: these are exactly the encodings a QEMU-based
//! model is least likely to get right, and the interpreter is the JIT's oracle, so
//! "interp == jit" proves nothing about them (both tiers share `f80.rs`).

//! **x86-64 Linux only.** Every case here compares against the REAL CPU through
//! `native::run_native`, which forks a child and executes the guest snippet on the host —
//! so on any other host there is nothing to compare against and the module does not even
//! exist. Gated at the file level rather than per test, because that is what every test
//! in it does.
//!
//! Discovered by the FIRST execution of the aarch64 CI lane (2026-08-14): the import was
//! unconditional, so the whole crate failed to compile on ARM. The pre-push cross-check
//! only ran `cargo check --target aarch64 -p x86jit-cranelift`, which never sees this
//! crate.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use iced_x86::code_asm::*;
use x86jit_core::InterpreterBackend;
use x86jit_tests::compare::compare;
use x86jit_tests::native::run_native;
use x86jit_tests::oracle::{run_with_backend, VectorInput};
use x86jit_tests::vector::{CpuSnapshot, MemChunk, MemKind, RunSpec};

const CODE: u64 = 0x21_0000;
const SCRATCH: u64 = 0x22_0000;

/// `fld tbyte [SCRATCH]`, `fld tbyte [SCRATCH+16]`, apply `op`, `fstp tbyte [SCRATCH+32]`.
/// Returns the 10 stored bytes from both engines.
fn binary(a: [u8; 10], b: [u8; 10], op: impl Fn(&mut CodeAssembler)) -> ([u8; 10], [u8; 10]) {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fld(tbyte_ptr(SCRATCH + 16)).unwrap(); // ST(1) after the next push
    asm.fld(tbyte_ptr(SCRATCH)).unwrap(); // ST(0)
    op(&mut asm);
    asm.fstp(tbyte_ptr(SCRATCH + 32)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut page = vec![0u8; 0x1000];
    page[..10].copy_from_slice(&a);
    page[16..26].copy_from_slice(&b);
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
    let out = |o: &x86jit_tests::oracle::RunOutcome| {
        let c = o.mem.iter().find(|c| c.addr == SCRATCH).unwrap();
        let mut r = [0u8; 10];
        r.copy_from_slice(&c.bytes[32..42]);
        r
    };
    let native = run_native(&input).expect("host runs an x87 snippet");
    let interp = run_with_backend(&input, Box::new(InterpreterBackend));
    assert!(
        compare(&native, &interp, &[]).is_none(),
        "interp diverges from the real CPU:\n{:#?}",
        compare(&native, &interp, &[])
    );
    (out(&native), out(&interp))
}

/// Build a 10-byte double-extended value from its parts.
fn enc(sign: bool, biased_exp: u16, significand: u64) -> [u8; 10] {
    let mut b = [0u8; 10];
    b[..8].copy_from_slice(&significand.to_le_bytes());
    let se = (biased_exp & 0x7fff) | ((sign as u16) << 15);
    b[8..].copy_from_slice(&se.to_le_bytes());
    b
}

const ONE: ([u8; 10], &str) = ([0, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x3F], "1.0");

/// SDM Vol 1 §8.2.2, Table 8-3: an unnormal, a pseudo-NaN and a pseudo-infinity are
/// unsupported operands. The 387 and later "generate an invalid-operation exception when
/// these encodings are encountered as operands", so masked they deliver the QNaN
/// indefinite — NOT the operand's payload, and not an ordinary finite result.
///
/// The engine used to call every non-zero, non-max exponent with a clear integer bit a
/// Normal, so an unnormal entered ordinary arithmetic and produced a plausible number.
#[test]
fn unsupported_encodings_yield_the_indefinite() {
    // sign, biased exponent, significand — integer bit (63) deliberately CLEAR.
    let cases: [([u8; 10], &str); 4] = [
        (
            enc(false, 0x4000, 0x4000_0000_0000_0000),
            "positive unnormal",
        ),
        (
            enc(true, 0x0001, 0x0000_0000_0000_0001),
            "negative unnormal, tiny exponent",
        ),
        (enc(false, 0x7FFF, 0x4000_0000_0000_0001), "pseudo-NaN"),
        (enc(false, 0x7FFF, 0x0000_0000_0000_0000), "pseudo-infinity"),
    ];
    // The double-extended QNaN indefinite (SDM Vol 1 §4.8.3.7, Table 4-3).
    let indefinite = enc(true, 0x7FFF, 0xC000_0000_0000_0000);
    for (bytes, what) in cases {
        let (native, interp) = binary(bytes, ONE.0, |a| a.faddp(st1, st0).unwrap());
        assert_eq!(native, indefinite, "hardware: {what} + 1.0");
        assert_eq!(interp, indefinite, "engine: {what} + 1.0");
    }
}

/// A pseudo-denormal is the one Table 8-3 entry that is *not* unsupported: "handled
/// correctly, considering the biased exponent as 1". Integer bit set, biased exponent 0.
#[test]
fn pseudo_denormals_are_ordinary_denormals() {
    let pd = enc(false, 0x0000, 0x8000_0000_0000_0000); // 1.0 × 2^-16382
                                                        // Adding it to itself must double it, not trap or indefinite.
    let (native, interp) = binary(pd, pd, |a| a.faddp(st1, st0).unwrap());
    assert_eq!(
        native, interp,
        "engine matches hardware on a pseudo-denormal"
    );
    assert_ne!(
        native,
        enc(true, 0x7FFF, 0xC000_0000_0000_0000),
        "a pseudo-denormal must not be treated as unsupported"
    );
    assert_ne!(native, [0u8; 10], "and must not flush to zero");
}

/// SDM Vol 1 §4.8.3.5, Table 4-8, the **X87 FPU** rows — which are not the SSE rows.
/// An SNaN paired with a QNaN yields the QNaN; two NaNs of a kind yield the one with the
/// larger significand; an SNaN is converted to a QNaN on the way out. Every arithmetic
/// arm used to return a bare `F80::nan()`, discarding sign, payload and quiet status.
#[test]
fn nan_identity_survives_arithmetic() {
    let qnan = |payload: u64| enc(false, 0x7FFF, 0xC000_0000_0000_0000 | payload);
    let snan = |payload: u64| enc(false, 0x7FFF, 0x8000_0000_0000_0000 | payload);

    // QNaN and a number → that QNaN, payload intact.
    let (native, interp) = binary(qnan(0x1234), ONE.0, |a| a.faddp(st1, st0).unwrap());
    assert_eq!(native, qnan(0x1234), "hardware keeps the QNaN payload");
    assert_eq!(interp, qnan(0x1234), "engine keeps the QNaN payload");

    // SNaN and a number → that SNaN quieted, payload intact.
    let (native, interp) = binary(snan(0x1234), ONE.0, |a| a.faddp(st1, st0).unwrap());
    assert_eq!(native, qnan(0x1234), "hardware quiets the SNaN in place");
    assert_eq!(interp, qnan(0x1234), "engine quiets the SNaN in place");

    // Two QNaNs → the larger significand, whichever operand it is on.
    let (native, interp) = binary(qnan(0x1111), qnan(0x9999), |a| a.faddp(st1, st0).unwrap());
    assert_eq!(
        native,
        qnan(0x9999),
        "hardware takes the larger significand"
    );
    assert_eq!(interp, qnan(0x9999), "engine takes the larger significand");
    let (native, interp) = binary(qnan(0x9999), qnan(0x1111), |a| a.faddp(st1, st0).unwrap());
    assert_eq!(native, qnan(0x9999), "…regardless of operand order");
    assert_eq!(interp, qnan(0x9999));

    // SNaN and QNaN → the QNaN, even though the SNaN has the larger significand.
    let (native, interp) = binary(snan(0x7FFF), qnan(0x0001), |a| a.faddp(st1, st0).unwrap());
    assert_eq!(
        native,
        qnan(0x0001),
        "hardware prefers the QNaN over the SNaN"
    );
    assert_eq!(
        interp,
        qnan(0x0001),
        "engine prefers the QNaN over the SNaN"
    );
}

/// The same rules through multiply and divide, so the fix is in the shared selection and
/// not pasted into one arm.
#[test]
fn nan_identity_holds_across_operations() {
    let qnan = |p: u64| enc(true, 0x7FFF, 0xC000_0000_0000_0000 | p);
    for (name, op) in [
        (
            "fmulp",
            (|a: &mut CodeAssembler| a.fmulp(st1, st0).unwrap()) as fn(&mut CodeAssembler),
        ),
        ("fdivp", |a: &mut CodeAssembler| a.fdivp(st1, st0).unwrap()),
        ("fsubp", |a: &mut CodeAssembler| a.fsubp(st1, st0).unwrap()),
    ] {
        let (native, interp) = binary(qnan(0xABCD), ONE.0, op);
        assert_eq!(native, qnan(0xABCD), "hardware, {name}");
        assert_eq!(interp, qnan(0xABCD), "engine, {name}");
    }
}
