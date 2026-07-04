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
