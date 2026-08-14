//! `#MF` delivery and the stack state around it (task-328 AC#3).
//!
//! Split out of `x87_exception_flags.rs` so it can run on EVERY host. That file compares
//! against the real CPU and is therefore x86-64-only; these assert against the
//! architecture directly, and one of them — the JIT variant — exercises
//! `trap_if_unmapped_or_exception` in generated code, which is exactly the thing an
//! AArch64 runner is there to execute.
//!
//! They cannot be host-witnessed anyway: an unmasked x87 exception makes the CPU raise
//! `#MF`, which reaches the native oracle's child as SIGFPE and kills it in a way the
//! `hlt`-#GP capture cannot report.

use iced_x86::code_asm::*;
use x86jit_core::{Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};

const ZE: u16 = 1 << 2;
const ES: u16 = 1 << 7;

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
