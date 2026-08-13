//! Fault atomicity (task-305, spec.md §16): an instruction may not commit any
//! guest-visible state until every read that can fault has succeeded.
//!
//! A precise fault leaves the destination unchanged and names the address that
//! actually faulted. Both halves are needed and they broke independently — one probe
//! caught each backend failing a different one:
//!
//! ```text
//! interp: UnmappedMemory{addr: 0x10000}  xmm0 1.0 -> 3.0   <- destination committed
//! jit:    UnmappedMemory{addr: 0xfff0}   xmm0 1.0 -> 1.0   <- base address, not the fault
//! ```
//!
//! **Why `jit_eq_interp` cannot check any of this.** The differential harness compares
//! two tiers that share the IR and, for the wide handlers, the shape of the bug. It also
//! compares state after a *completed* run; a trapped instruction's partial state is
//! exactly what it does not look at. So these assert against the architecture directly.
//!
//! The 256-bit shape matters because `dst == src1` is legal and common
//! (`vaddps ymm0, ymm0, [mem]`): committing the low half and then faulting on the high
//! one means the retry reads its own output as the source and adds twice.

use iced_x86::code_asm::*;
use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const SPAN: u64 = 0x1_0000;
const CODE: u64 = 0x1000;
/// The last 16 bytes of the guest span: a 32-byte operand here has its low half in
/// bounds and its high half past the end.
///
/// Out of span, not in-span-but-unmapped, because those are different on the two tiers:
/// the JIT bounds-checks against `MemCtx.size` alone and has no region map, so an
/// in-span unmapped read is demand-zero there and never faults (decision-3; the fix is
/// guard pages in the embedder, decision-5). Measured, not assumed — the same probe
/// with the operand inside the span returned `Hlt` on the JIT and `UnmappedMemory` on
/// the interpreter. Out of span is where both tiers must agree.
const STRADDLE: u64 = SPAN - 16;

const ONE: f32 = 1.0;
const TWO: f32 = 2.0;

fn splat(v: f32) -> u128 {
    let b = v.to_bits() as u128;
    b | (b << 32) | (b << 64) | (b << 96)
}

/// `vaddps ymm0, ymm0, [rax]` — destination aliases source 1, second source in memory.
fn run_straddling_vaddps(backend: Box<dyn Backend>) -> (Exit, u128) {
    let mut vm = Vm::with_backend(VmConfig::flat(SPAN), backend);
    vm.map(0, SPAN as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();

    let mut a = CodeAssembler::new(64).unwrap();
    a.vaddps(ymm0, ymm0, ymmword_ptr(rax)).unwrap();
    a.hlt().unwrap();
    vm.write_bytes(CODE, &a.assemble(CODE).unwrap()).unwrap();
    vm.write_bytes(STRADDLE, &splat(TWO).to_le_bytes()).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rax, STRADDLE);
    cpu.set_xmm(0, splat(ONE));
    let exit = cpu.run(&vm, None);
    (exit, cpu.xmm(0))
}

/// The destination must be untouched when the high half faults.
fn destination_survives(backend: Box<dyn Backend>) {
    let (exit, dst) = run_straddling_vaddps(backend);
    assert!(
        matches!(exit, Exit::UnmappedMemory { .. }),
        "expected a fault on the high half, got {exit:?}"
    );
    assert_eq!(
        dst,
        splat(ONE),
        "the low half was committed before the high half faulted; with dst == src1 the \
         retry would read its own output and add twice"
    );
}

#[test]
fn straddling_vaddps_leaves_the_destination_untouched_interp() {
    destination_survives(Box::new(InterpreterBackend));
}

#[test]
fn straddling_vaddps_leaves_the_destination_untouched_jit() {
    destination_survives(Box::new(JitBackend::new()));
}

/// The reported address must be the sub-access that faulted, not the operand base.
///
/// A 32-byte access is four 8-byte operations; reporting the base tells the embedder to
/// map a page it has already mapped. It retries, faults identically, and loops — and it
/// cannot work around this, because the information was discarded before the `Exit` was
/// built.
fn fault_names_the_failing_half(backend: Box<dyn Backend>) {
    let (exit, _) = run_straddling_vaddps(backend);
    match exit {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(
            addr, SPAN,
            "reported the operand base {STRADDLE:#x} instead of the faulting half \
             {SPAN:#x}; the embedder is told to map a page it already has"
        ),
        other => panic!("expected UnmappedMemory, got {other:?}"),
    }
}

#[test]
fn straddling_vaddps_names_the_failing_half_interp() {
    fault_names_the_failing_half(Box::new(InterpreterBackend));
}

#[test]
fn straddling_vaddps_names_the_failing_half_jit() {
    fault_names_the_failing_half(Box::new(JitBackend::new()));
}

/// A faulting 256-bit op must be RESUMABLE: after the embedder supplies the missing
/// memory, re-running the instruction produces the architectural result exactly once.
///
/// Interpreter only, and not by preference. Resuming needs the faulting address to
/// become valid, which needs it to be inside the guest span; and inside the span the
/// JIT does not fault at all (see [`STRADDLE`]). So the JIT cannot produce a resumable
/// RAM fault in this configuration — that is decision-3, not a gap in this test. What
/// the JIT *can* be held to is the two assertions above, and it is.
#[test]
fn a_faulting_256bit_op_is_resumable_interp() {
    const BIG: u64 = 0x2_0000;
    const OPERAND: u64 = SPAN - 16; // low half mapped, high half in span but unmapped

    let mut vm = Vm::with_backend(VmConfig::flat(BIG), Box::new(InterpreterBackend));
    vm.map(0, SPAN as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();

    let mut a = CodeAssembler::new(64).unwrap();
    a.vaddps(ymm0, ymm0, ymmword_ptr(rax)).unwrap();
    a.hlt().unwrap();
    vm.write_bytes(CODE, &a.assemble(CODE).unwrap()).unwrap();
    vm.write_bytes(OPERAND, &splat(TWO).to_le_bytes()).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rax, OPERAND);
    cpu.set_xmm(0, splat(ONE));
    cpu.set_ymm_hi(0, splat(ONE));

    let exit = cpu.run(&vm, None);
    let Exit::UnmappedMemory { addr, .. } = exit else {
        panic!("expected a fault on the high half, got {exit:?}");
    };
    assert_eq!(addr, SPAN, "must name the faulting half");

    // The embedder answers the fault, exactly as it would for demand paging.
    vm.map(SPAN, 0x1000, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(SPAN, &splat(TWO).to_le_bytes()).unwrap();

    match cpu.run(&vm, None) {
        Exit::Hlt => {}
        other => panic!("retry did not complete: {other:?}"),
    }
    assert_eq!(
        cpu.xmm(0),
        splat(ONE + TWO),
        "low lanes added twice — the first attempt committed them before faulting"
    );
    assert_eq!(cpu.ymm_hi(0), splat(ONE + TWO), "high lanes");
}

/// A three-operand VEX op whose second source is memory must not touch its destination
/// when that load faults.
///
/// `VPackWideM`, `VUnpackLowM` and `VHIntM` are declared as in-place
/// `dst = op(dst, [addr])` and rely on the LIFTER emitting a `VMov dst, a` first. That
/// copy lands before the faulting load, so a fault leaves `dst` holding `a`. The result
/// after a retry is still right — `a` is intact in its own register — but the
/// architectural state at the fault is not, which is what a debugger, a guest fault
/// handler, or anything else that inspects registers mid-trap actually reads.
///
/// `VHFloatM` carries its first source explicitly and is the shape that does not have
/// this problem; the point of fixing the op rather than only the interpreter is that the
/// IR must not keep inviting the next backend to reintroduce it.
fn three_operand_vex_mem_leaves_dst_untouched(backend: Box<dyn Backend>) {
    let mut vm = Vm::with_backend(VmConfig::flat(SPAN), backend);
    vm.map(0, SPAN as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();

    // 16-byte operand whose second 8-byte half is past the end of the guest span.
    const HALF: u64 = SPAN - 8;
    let mut a = CodeAssembler::new(64).unwrap();
    a.vpunpcklbw(xmm1, xmm2, xmmword_ptr(rax)).unwrap();
    a.hlt().unwrap();
    vm.write_bytes(CODE, &a.assemble(CODE).unwrap()).unwrap();

    const SENTINEL: u128 = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_FF00;
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rax, HALF);
    cpu.set_xmm(1, SENTINEL);
    cpu.set_xmm(2, 0);

    let exit = cpu.run(&vm, None);
    // The operand is 16 bytes at SPAN-8, so its FIRST 8-byte half is in bounds and the
    // second is not. This is the sub-address `vload`/`vstore` used to discard: they do
    // two 8-byte accesses and only the `MemTrap` came back, so the caller reported the
    // operand base. Nothing else in the suite covers that inner half — the 256-bit tests
    // above split at 16 bytes, and the `vextract` straddle test stores lane-by-lane.
    match exit {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(
            addr, SPAN,
            "reported the operand base {HALF:#x} instead of the faulting 8-byte half"
        ),
        other => panic!("expected a fault on the second half, got {other:?}"),
    }
    assert_eq!(
        cpu.xmm(1),
        SENTINEL,
        "the destination was written before the faulting load"
    );
}

#[test]
fn three_operand_vex_mem_leaves_dst_untouched_interp() {
    three_operand_vex_mem_leaves_dst_untouched(Box::new(InterpreterBackend));
}

#[test]
fn three_operand_vex_mem_leaves_dst_untouched_jit() {
    three_operand_vex_mem_leaves_dst_untouched(Box::new(JitBackend::new()));
}

/// A >8-byte vector access to a `Trap` region traps forever — RECORDED, not fixed.
///
/// This asserts the defect on purpose, the way `x87_faults.rs` once pinned a divergent
/// tag word, so that fixing it fails this test and forces the record to be updated
/// rather than leaving a stale claim behind.
///
/// Measured: four rounds of `run` → `complete_mmio_read` produce four identical
/// `MmioRead { addr, size: 16 }` exits. The vector path re-calls `vload` unconditionally
/// and never consumes what the embedder supplied.
///
/// It is not a one-line fix, which is why it is `TASK-332` and not part of task-305. A
/// 16-byte access is TWO 8-byte transfers, and `Exit::MmioRead`'s answer channel
/// (`complete_mmio_read(u64)`) carries one. Consuming one pending value per retry cannot
/// converge: answer the second half, re-enter, and the first half traps again with
/// nothing pending. Completing it needs either per-instruction progress state or a
/// defined refusal — an embedder-visible contract change either way.
#[test]
fn vector_mmio_read_cannot_be_completed_yet() {
    const MMIO: u64 = 0x4000;

    let mut vm = Vm::with_backend(VmConfig::flat(SPAN), Box::new(InterpreterBackend));
    vm.map(0, MMIO as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();
    vm.map(MMIO, 0x1000, Prot::RW, RegionKind::Trap).unwrap();
    vm.map(
        MMIO + 0x1000,
        (SPAN - MMIO - 0x1000) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();

    let mut a = CodeAssembler::new(64).unwrap();
    a.movdqu(xmm0, xmmword_ptr(rax)).unwrap();
    a.hlt().unwrap();
    vm.write_bytes(CODE, &a.assemble(CODE).unwrap()).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rax, MMIO);

    for round in 0..3 {
        match cpu.run(&vm, None) {
            Exit::MmioRead { addr, size } => {
                assert_eq!((addr, size), (MMIO, 16), "round {round}");
                cpu.complete_mmio_read(0x1234);
            }
            other => panic!(
                "round {round}: TASK-332 appears to be fixed ({other:?}) — update this \
                 test and the records in README/deferred.md that still call it broken"
            ),
        }
    }
}
