//! Self-modifying code (M6, spec §10, testing.md §6): a write onto a page that
//! backs a translated block must invalidate the cache so the next execution
//! re-lifts the changed bytes. Two write sources are covered:
//!
//! - the guest patching its own `.text` (interpreter store path), and
//! - an embedder overwriting code between runs (`write_bytes` — loader / syscall
//!   passthrough), which works on both backends.
//!
//! JIT-compiled guest stores write host RAM directly (§8.2.1) and don't route
//! through this hook — faithful JIT-side SMC is the deferred "mark host code
//! dead" step (§10), so the guest-self-patch case is asserted on the interpreter.

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x1_0000;
const MAIN: u64 = 0x1000;
const TARGET: u64 = 0x2000;
const STACK_TOP: u64 = 0x8000;

fn assemble(origin: u64, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    build(&mut a);
    a.assemble(origin).unwrap()
}

fn new_vm(backend: Box<dyn Backend>) -> Vm {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
    vm
}

fn run_to_hlt(vm: &Vm, cpu: &mut x86jit_core::Vcpu) {
    match cpu.run(vm, None) {
        Exit::Hlt => {}
        other => panic!("unexpected exit: {other:?}"),
    }
}

/// The guest overwrites its own code: it calls `target` (caching that block),
/// patches `target`'s first instruction from `mov eax, 1` to `mov eax, 2`, then
/// calls `target` again. Without SMC invalidation the second call would replay
/// the stale cached `eax = 1`; with it, the engine re-lifts and yields `eax = 2`.
#[test]
fn interpreter_observes_guest_self_modification() {
    let mut vm = new_vm(Box::new(InterpreterBackend));

    // target: `mov eax, 1; ret`  ->  B8 01 00 00 00 C3
    let target = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.ret().unwrap();
    });
    vm.write_bytes(TARGET, &target).unwrap();

    let main = assemble(MAIN, |a| {
        a.mov(r15, TARGET).unwrap();
        a.call(r15).unwrap(); // run target v1 (eax = 1), caches its block
                              // patch target's first 5 bytes to `mov eax, 2` (B8 02 00 00 00)
        a.mov(byte_ptr(TARGET), 0xB8i32).unwrap();
        a.mov(dword_ptr(TARGET + 1), 2i32).unwrap();
        a.call(r15).unwrap(); // run target v2 — must observe eax = 2
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    cpu.set_reg(Reg::Rsp, STACK_TOP);
    run_to_hlt(&vm, &mut cpu);

    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        2,
        "second call must run the patched code"
    );
    assert!(
        vm.cache.misses() >= 2,
        "target must have been lifted twice (initial + re-lift)"
    );
}

/// An embedder overwrites a cached block between runs via `write_bytes` (the
/// loader / syscall-passthrough path). This works on both backends — the write
/// routes through the SMC hook regardless of who executes the code.
fn embedder_rewrite_reexecutes(backend: Box<dyn Backend>) {
    let mut vm = new_vm(backend);

    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");

    // Overwrite the block with `mov eax, 42; hlt` and re-run from the same entry.
    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "re-run must see the rewritten code"
    );
}

#[test]
fn embedder_rewrite_reexecutes_interp() {
    embedder_rewrite_reexecutes(Box::new(InterpreterBackend));
}

#[test]
fn embedder_rewrite_reexecutes_jit() {
    embedder_rewrite_reexecutes(Box::new(JitBackend::new()));
}

/// A *chained* edge must survive SMC: block MAIN ends in a direct `jmp TARGET`,
/// so after the first run its link slot points at TARGET's compiled entry. When
/// the embedder rewrites TARGET (a different code page — MAIN's block is NOT
/// invalidated), the next run of MAIN must NOT chain into TARGET's stale compiled
/// code. Requires `handle_smc` to clear the backend's link slots on invalidation
/// (otherwise the filled slot returns `RET_CHAIN` into the dropped block). JIT
/// only — the interpreter has no link slots.
#[test]
fn stale_link_slot_cleared_on_invalidation() {
    let mut vm = new_vm(Box::new(JitBackend::new()));

    // MAIN (page 0x1000): jump straight to TARGET (a direct, chainable edge).
    let main = assemble(MAIN, |a| {
        a.jmp(TARGET).unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    // TARGET (page 0x2000): mov eax, 1; hlt.
    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    // First run: MAIN links to TARGET (slot filled), TARGET yields eax = 1.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");

    // Embedder rewrites ONLY TARGET (mov eax, 42; hlt). MAIN's page is untouched,
    // so MAIN's compiled block — and its filled link slot — survive.
    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();
    let misses_before = vm.cache.misses();

    // Second run from MAIN: the stale slot must not be followed. With the fix,
    // SMC clears the slot, MAIN re-links, TARGET is re-lifted → eax = 42.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "chained edge must re-resolve the rewritten TARGET, not run stale code"
    );
    assert!(
        vm.cache.misses() > misses_before,
        "TARGET must have been re-lifted after invalidation"
    );
}

/// The per-vcpu fast-resolve cache (fast-dispatch R3) must not outlive an invalidation:
/// the SAME vcpu runs a block, the embedder rewrites it, and the vcpu runs it
/// again. Without the invalidation-epoch flush the vcpu's fast cache would serve
/// the stale compiled entry; with it, the cache flushes and the block re-lifts.
#[test]
fn fast_resolve_cache_flushes_on_invalidation() {
    let mut vm = new_vm(Box::new(JitBackend::new()));

    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    // One vcpu, reused across both runs, so its fast-resolve cache persists.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");

    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();
    let misses_before = vm.cache.misses();

    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "same vcpu must re-lift the rewritten block, not serve its stale fast entry"
    );
    assert!(
        vm.cache.misses() > misses_before,
        "the rewritten block must have been re-lifted"
    );
}

/// IBTC slots (fast-dispatch R4) inherit the same SMC coherence as link slots: an
/// indirect `jmp reg` fills a per-site descriptor pointing at TARGET's compiled
/// entry; when the embedder rewrites TARGET, the next run must not chain through
/// the stale descriptor. `invalidate_links` zeroes the IBTC slot (in the same
/// arena as link slots), so the site re-resolves. JIT only.
#[test]
fn stale_ibtc_descriptor_cleared_on_invalidation() {
    let mut vm = new_vm(Box::new(JitBackend::new()));

    // MAIN (page 0x1000): mov rdx, TARGET; jmp rdx  — a monomorphic indirect jump.
    let main = assemble(MAIN, |a| {
        a.mov(rdx, TARGET).unwrap();
        a.jmp(rdx).unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    // First run: the jmp reg fills its IBTC slot with {TARGET, v1 entry}, eax = 1.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");
    assert!(vm.cache.ibtc_filled() >= 1, "IBTC must have fired");

    // Rewrite ONLY TARGET; MAIN (and its filled IBTC slot) survive.
    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "indirect edge must re-resolve the rewritten TARGET, not run stale code"
    );
}

/// A write to a NON-code page must not perturb the cache (no false invalidation).
#[test]
fn write_to_data_page_does_not_invalidate() {
    let mut vm = new_vm(Box::new(InterpreterBackend));
    let code = assemble(TARGET, |a| {
        a.mov(eax, 7i32).unwrap();
        a.mov(dword_ptr(0x4000u64), eax).unwrap(); // store to a far data page
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    let misses_after_first = vm.cache.misses();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        vm.cache.misses(),
        misses_after_first,
        "data-page write must not re-lift code"
    );
    assert!(vm.cache.hits() >= 1, "second run should hit the cache");
}
