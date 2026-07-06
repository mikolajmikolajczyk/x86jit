//! Background tier-up (bg-tier, doc-27 BGT-3): the dispatcher submits a hot block to
//! the backend's compiler thread and swaps it in when it lands, instead of compiling
//! inline. Opt-in (`Vm::set_tier_up_background`), so these tests drive it explicitly;
//! the default-off corpus is unaffected (AC#4). Determinism comes from
//! `JitBackend::tier_up_handle().wait_idle()` — no sleeps.

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;

const CODE: u64 = 0x1000;

fn vm_with(backend: Box<dyn Backend>, tier: u32, background: bool, prog: &[u8]) -> Vm {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: 0x2000 },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.set_tier_up_after(Some(tier));
    vm.set_tier_up_background(background);
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, prog).unwrap();
    vm
}

/// `mov eax, 42 ; hlt` — one block, re-runnable by resetting RIP.
fn single_block() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    a.mov(eax, 42i32).unwrap();
    a.hlt().unwrap();
    a.assemble(CODE).unwrap()
}

/// A counted accumulation loop: eax = sum(1..=n). Its body block runs `n` times, so it
/// tiers up and (under background mode) switches to compiled mid-run.
fn accumulate_loop(n: i32) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut top = a.create_label();
    a.mov(eax, 0i32).unwrap();
    a.mov(ecx, n).unwrap();
    a.set_label(&mut top).unwrap();
    a.add(eax, ecx).unwrap();
    a.sub(ecx, 1i32).unwrap();
    a.jnz(top).unwrap();
    a.hlt().unwrap();
    a.assemble(CODE).unwrap()
}

/// Run to `hlt` and return RAX.
fn run_to_hlt(vm: &Vm) -> u64 {
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(vm, Some(50_000_000)), Exit::Hlt), "hlts");
    cpu.reg(Reg::Rax)
}

/// One pass through the single block (reset RIP, run to hlt), returning RAX.
fn one_pass(vm: &Vm, cpu: &mut x86jit_core::Vcpu) -> u64 {
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(vm, Some(1_000_000)), Exit::Hlt), "hlts");
    cpu.reg(Reg::Rax)
}

/// AC#1: the deterministic tier-up recipe (doc-27 D6). With threshold 3 and background
/// on, the block stays interpreted (published == 0) through the submit, `wait_idle`
/// compiles it off-thread, and the next dispatch publishes it (published == 1) — with
/// RAX identical to the interpreter throughout, no sleeps or timing.
#[test]
fn deterministic_background_tier_up() {
    let prog = single_block();
    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let vm = vm_with(Box::new(jit), 3, true, &prog);
    let mut cpu = vm.new_vcpu();

    // Four passes: run1 caches (no bump), runs 2-3 bump to 2 (< 3), run4 bumps to 3 and
    // submits the compile in the background. Still interpreted, nothing published.
    for _ in 0..4 {
        assert_eq!(one_pass(&vm, &mut cpu), 42);
    }
    assert_eq!(
        vm.cache.tier_bg_published(),
        0,
        "submitted, not yet published"
    );

    handle.wait_idle(); // the worker finishes the compile — still not published
    assert_eq!(
        vm.cache.tier_bg_published(),
        0,
        "compiled, awaiting a drain"
    );

    // The next dispatch drains the completion and publishes it.
    assert_eq!(one_pass(&vm, &mut cpu), 42);
    assert_eq!(
        vm.cache.tier_bg_published(),
        1,
        "published on the next dispatch"
    );
    assert_eq!(vm.cache.tier_bg_rejected(), 0, "no stale rejections");

    // Final state matches the interpreter oracle.
    assert_eq!(
        run_to_hlt(&vm_with(Box::new(InterpreterBackend), 3, true, &prog)),
        42
    );
}

/// AC#2: a real loop under background tier-up produces the interpreter's result and
/// actually publishes at least one background compile (the body block tiers up mid-run).
#[test]
fn real_loop_background_matches_interp_and_publishes() {
    let prog = accumulate_loop(100_000);
    let interp = run_to_hlt(&vm_with(Box::new(InterpreterBackend), 2, false, &prog));

    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let vm = vm_with(Box::new(jit), 2, true, &prog);
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(&vm, Some(50_000_000)), Exit::Hlt));
    handle.wait_idle(); // drain any straggler so the count is stable

    assert_eq!(
        cpu.reg(Reg::Rax),
        interp,
        "background result matches interp"
    );
    assert!(
        vm.cache.tier_bg_published() > 0,
        "the hot loop body tiered up in the background"
    );
}

/// AC#5: the interpreter backend with the background flag on returns `Unsupported` from
/// `tier_up_async`, so a hot block falls through to inline tier-up — behaving exactly
/// like the flag-off path (identical result, nothing published in the background).
#[test]
fn interp_backend_background_falls_back_to_inline() {
    let prog = accumulate_loop(1_000);
    let off = run_to_hlt(&vm_with(Box::new(InterpreterBackend), 2, false, &prog));
    let bg_vm = vm_with(Box::new(InterpreterBackend), 2, true, &prog);
    let on = run_to_hlt(&bg_vm);
    assert_eq!(on, off, "bg flag is a no-op on the interpreter backend");
    assert_eq!(
        bg_vm.cache.tier_bg_published(),
        0,
        "interp never publishes bg"
    );
}
