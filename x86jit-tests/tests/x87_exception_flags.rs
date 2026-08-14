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

// ---------------------------------------------------------------------------
// #MF delivery (task-328 AC#3)
//
// These assert against the architecture rather than against the host, and the reason is
// worth stating: an unmasked x87 exception makes the CPU raise #MF, which reaches the
// native oracle's child as SIGFPE and kills it in a way the `hlt`-#GP capture cannot
// report. Everything above IS host-witnessed; this is the part that cannot be.
// ---------------------------------------------------------------------------

use x86jit_core::{Exit, Prot, Reg, RegionKind, Vm, VmConfig};

/// `1.0 / 0.0` with ZE UNMASKED, followed by another x87 instruction.
///
/// The rule under test is the one that makes #MF confusing to implement: the FPU signals
/// on the faulting instruction, but the processor "checks the ES flag ... on the NEXT
/// occurrence of a floating-point instruction or a WAIT/FWAIT" and traps there (SDM Vol 1
/// §8.6). So the reported RIP is the *following* op — asserting merely that "an exception
/// arrives" would pass with the report on the wrong instruction, which is exactly the bug
/// the deferral exists to avoid.
fn run_unmasked_divide_on(
    mask_ze: bool,
    backend: Box<dyn x86jit_core::Backend>,
) -> (Exit, u64, u64) {
    const SPAN: u64 = 0x1_0000;
    const CODE: u64 = 0x1000;
    const DATA: u64 = 0x3000;

    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(DATA)).unwrap();
    asm.fld1().unwrap(); // ST(0) = 1.0
    asm.fldz().unwrap(); // ST(0) = 0.0, ST(1) = 1.0
    asm.fdivp(st1, st0).unwrap(); // ST(1) = 1.0 / 0.0  -> #Z
    let before_next = asm.assemble(CODE).unwrap().len() as u64;
    asm.fld1().unwrap(); // the next WAITING op — the trap belongs here
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();
    let next_op = CODE + before_next;

    let cw: u16 = if mask_ze { 0x037F } else { 0x037F & !(1 << 2) };
    let mut vm = Vm::with_backend(VmConfig::flat(SPAN), backend);
    vm.map(0, SPAN as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    vm.write_bytes(DATA, &cw.to_le_bytes()).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    let exit = cpu.run(&vm, None);
    (exit, cpu.reg(Reg::Rip), next_op)
}

fn run_unmasked_divide(mask_ze: bool) -> (Exit, u64, u64) {
    run_unmasked_divide_on(mask_ze, Box::new(InterpreterBackend))
}

fn traps_on_the_following_instruction(backend: fn() -> Box<dyn x86jit_core::Backend>) {
    let (exit, rip, next_op) = run_unmasked_divide_on(false, backend());
    match exit {
        Exit::Exception { vector, addr } => {
            assert_eq!(vector, 16, "#MF is vector 16");
            assert_eq!(
                addr, next_op,
                "reported on the instruction that RAISED it, not the following one — \
                 the guest's handler would see RIP an instruction early"
            );
            assert_eq!(rip, next_op, "RIP must sit on the trapping instruction");
        }
        other => panic!("expected #MF, got {other:?}"),
    }
}

#[test]
fn an_unmasked_exception_traps_on_the_following_instruction_interp() {
    traps_on_the_following_instruction(|| Box::new(InterpreterBackend));
}

/// The JIT routes x87 through a helper, so the check lives in a second place and is
/// asserted rather than assumed — "it shares the path" is what made the SMC gap invisible
/// for so long.
#[test]
fn an_unmasked_exception_traps_on_the_following_instruction_jit() {
    traps_on_the_following_instruction(|| Box::new(x86jit_cranelift::JitBackend::new()));
}

/// The same program with ZE masked must run to completion. Without this, a test that
/// only checked the unmasked case would also pass on an engine that trapped always.
#[test]
fn a_masked_exception_does_not_trap() {
    let (exit, _, _) = run_unmasked_divide(true);
    assert!(
        matches!(exit, Exit::Hlt),
        "a masked exception returns a result, it does not trap: {exit:?}"
    );
}

/// A handler must be able to READ the status word and CLEAR it without trapping.
///
/// FNSTSW and FNCLEX are two of the six non-waiting instructions (SDM Vol 1 §8.6:
/// "FNINIT, FNSTENV, FNSAVE, FNSTSW, FNSTCW, and FNCLEX"). If they took the implicit
/// wait like every other x87 op, a guest that unmasked an exception would trap, enter its
/// handler, trap again on the first instruction of the handler, and never get out. This
/// is the property that makes the exclusion list load-bearing rather than trivia.
#[test]
fn a_handler_can_read_and_clear_the_status_word() {
    const SPAN: u64 = 0x1_0000;
    const CODE: u64 = 0x1000;
    const DATA: u64 = 0x3000;
    const OUT: u64 = 0x3008;

    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fldcw(word_ptr(DATA)).unwrap(); // unmask ZE
    asm.fld1().unwrap();
    asm.fldz().unwrap();
    asm.fdivp(st1, st0).unwrap(); // raises #Z, ES set, instruction abandoned
                                  // What a handler does first: read the status word, then clear it. Neither waits.
    asm.fnstsw(word_ptr(OUT)).unwrap();
    asm.fnclex().unwrap();
    // ...and now an ordinary waiting op must run, because ES is clear again.
    asm.fld1().unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let cw: u16 = 0x037F & !(1 << 2);
    let mut vm = Vm::with_backend(VmConfig::flat(SPAN), Box::new(InterpreterBackend));
    vm.map(0, SPAN as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    vm.write_bytes(DATA, &cw.to_le_bytes()).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    let exit = cpu.run(&vm, None);
    assert!(
        matches!(exit, Exit::Hlt),
        "the handler could not get out of its own trap: {exit:?}"
    );

    let mut sw = [0u8; 2];
    vm.read_bytes(OUT, &mut sw).unwrap();
    let sw = u16::from_le_bytes(sw);
    assert_eq!(
        sw & ZE,
        ZE,
        "fnstsw must report the flag it was called to read"
    );
    assert_eq!(sw & ES, ES, "...with the summary set, since ZE is unmasked");
}

/// An unmasked exception ABANDONS the instruction: no result is written and TOP does not
/// move (SDM Vol 1 §8.6, "stops further execution of the floating-point instruction";
/// §8.5.1.1 for the stack cases, "the top-of-stack pointer (TOP) and source operands
/// remain unaltered").
///
/// Observing that needs a non-waiting instruction, which is what makes this test possible
/// at all: `fnstenv` dumps the environment WITHOUT taking the implicit wait, so it runs
/// even with ES pending and reports the TOP the abandoned `fdivp` left behind.
///
/// It exists because deleting the abandon-on-unmasked logic broke no test — the eleven
/// above all watch flags, and flags are set either way.
#[test]
fn an_unmasked_exception_leaves_the_stack_untouched() {
    const SPAN: u64 = 0x1_0000;
    const CODE: u64 = 0x1000;
    const DATA: u64 = 0x3000;
    const ENV: u64 = 0x4000;

    let run = |cw: u16| -> u32 {
        let mut asm = CodeAssembler::new(64).unwrap();
        asm.fldcw(word_ptr(DATA)).unwrap();
        asm.fld1().unwrap(); // TOP 0 -> 7
        asm.fldz().unwrap(); // TOP 7 -> 6
        asm.fdivp(st1, st0).unwrap(); // would pop: TOP 6 -> 7
        asm.fnstenv(dword_ptr(ENV)).unwrap(); // non-waiting: runs regardless
        asm.hlt().unwrap();
        let code = asm.assemble(CODE).unwrap();

        let mut vm = Vm::with_backend(VmConfig::flat(SPAN), Box::new(InterpreterBackend));
        vm.map(0, SPAN as usize, Prot::RWX, RegionKind::Ram)
            .unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        vm.write_bytes(DATA, &cw.to_le_bytes()).unwrap();
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        assert!(matches!(cpu.run(&vm, None), Exit::Hlt));

        // 28-byte environment, 32-bit layout: status word at offset 4, TOP in bits 11-13.
        let mut env = [0u8; 6];
        vm.read_bytes(ENV, &mut env).unwrap();
        let sw = u16::from_le_bytes([env[4], env[5]]);
        ((sw >> 11) & 7) as u32
    };

    // Masked: the divide completes and pops, so TOP advances 6 -> 7.
    assert_eq!(run(0x037F), 7, "a masked exception still produces a result");
    // Unmasked: the divide is abandoned, so TOP stays where the two pushes left it.
    assert_eq!(
        run(0x037F & !(1 << 2)),
        6,
        "the abandoned instruction popped anyway — a handler would find TOP and its \
         source operands already destroyed"
    );
}
