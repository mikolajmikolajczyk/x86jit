//! Multi-vcpu store/atomic coherence (task-165). A plain guest store racing an atomic
//! RMW/CAS on the same location must stay coherent — the interpreter used to go through
//! `as_mut_slice()` (a `&mut [u8]` over the whole shared backing), which is
//! mutable-aliasing UB against another vcpu's atomic access; the optimizer then reordered
//! or elided the plain store relative to the atomic op, breaking mutual exclusion. These
//! run real multi-vcpu contention over one `Arc<Vm>` and assert no updates are lost.
//!
//! The headline case is `plain_store_release_spinlock_excludes`: a textbook x86 spinlock
//! (atomic `lock cmpxchg` acquire + PLAIN-STORE release) guarding a plain increment. On
//! the pre-fix engine two vcpus entered the critical section at once and the plain `inc`
//! lost updates; with the fix the lock excludes and the count is exact.

use std::sync::Arc;
use std::thread;

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vcpu,
    Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x40_0000;
const CODE: u64 = 0x1000;
const WORD: u64 = 0xA000;
const THREADS: u64 = 8;
const ITERS: u64 = 50_000;

fn vm_with(backend: Box<dyn Backend>, code: &[u8]) -> Arc<Vm> {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, code).unwrap();
    Arc::new(vm)
}

/// Run `THREADS` vcpus from `CODE` to `hlt` over one shared `Vm`.
fn run_all(vm: &Arc<Vm>, setup: impl Fn(&mut Vcpu, u64) + Send + Sync + Copy + 'static) {
    let handles: Vec<_> = (0..THREADS)
        .map(|id| {
            let vm = Arc::clone(vm);
            thread::spawn(move || {
                let mut cpu = vm.new_vcpu();
                cpu.set_reg(Reg::Rip, CODE);
                setup(&mut cpu, id);
                match cpu.run(&vm, None) {
                    Exit::Hlt => {}
                    other => panic!("thread {id}: {other:?}"),
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

fn read_u64(vm: &Vm, addr: u64) -> u64 {
    let mut b = [0u8; 8];
    vm.read_bytes(addr, &mut b).unwrap();
    u64::from_le_bytes(b)
}

/// A cmpxchg-acquire + **plain-store** release spinlock guarding a plain `inc`. If the
/// lock excludes, exactly `THREADS * ITERS` increments land; the pre-fix engine lost
/// updates because the plain-store release was reordered/elided against the acquire CAS
/// (task-165), letting two vcpus into the critical section.
fn plain_store_release_spinlock(backend: Box<dyn Backend>) {
    const LOCK: u64 = 0xB000;
    const CTR: u64 = 0xB008;
    let mut a = CodeAssembler::new(64).unwrap();
    let mut top = a.create_label();
    a.mov(rcx, ITERS).unwrap();
    a.set_label(&mut top).unwrap(); // acquire/retry + loop back-edge
    a.xor(rax, rax).unwrap(); // expected 0
    a.mov(rdx, 1u64).unwrap(); // desired 1
    a.mov(rbx, LOCK).unwrap();
    a.lock().cmpxchg(qword_ptr(rbx), rdx).unwrap(); // ZF=1 iff acquired
    a.jnz(top).unwrap();
    // critical section: plain (unlocked) increment — safe only under real exclusion.
    a.mov(rbx, CTR).unwrap();
    a.inc(qword_ptr(rbx)).unwrap();
    // release: plain store 0 (imm64→mem isn't encodable, so via a zeroed reg).
    a.xor(r8, r8).unwrap();
    a.mov(rbx, LOCK).unwrap();
    a.mov(qword_ptr(rbx), r8).unwrap();
    a.dec(rcx).unwrap();
    a.jnz(top).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();
    let vm = vm_with(backend, &code);
    run_all(&vm, |_, _| {});
    assert_eq!(
        read_u64(&vm, CTR),
        THREADS * ITERS,
        "plain-store-release spinlock failed to exclude: the critical-section inc lost \
         updates (plain store reordered/elided vs the acquire CAS — task-165)"
    );
}

#[test]
fn plain_store_release_spinlock_excludes_interp() {
    plain_store_release_spinlock(Box::new(InterpreterBackend));
}

#[test]
fn plain_store_release_spinlock_excludes_jit() {
    plain_store_release_spinlock(Box::new(JitBackend::new()));
}

/// Direct `lock cmpxchg` CAS-increment loop (the canonical lock-free counter): validates
/// that AtomicCas is atomic with a correct ZF under contention on both backends.
fn cas_increment_counter(backend: Box<dyn Backend>) {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut retry = a.create_label();
    a.mov(rcx, ITERS).unwrap();
    a.mov(rbx, WORD).unwrap();
    a.set_label(&mut retry).unwrap();
    a.mov(rax, qword_ptr(rbx)).unwrap(); // expected = current
    a.mov(rdx, rax).unwrap();
    a.inc(rdx).unwrap(); // desired = current + 1
    a.lock().cmpxchg(qword_ptr(rbx), rdx).unwrap();
    a.jnz(retry).unwrap(); // raced → reload, no counter change
    a.dec(rcx).unwrap();
    a.jnz(retry).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();
    let vm = vm_with(backend, &code);
    run_all(&vm, |_, _| {});
    assert_eq!(
        read_u64(&vm, WORD),
        THREADS * ITERS,
        "CAS-increment lost updates"
    );
}

#[test]
fn cas_increment_counter_interp() {
    cas_increment_counter(Box::new(InterpreterBackend));
}

#[test]
fn cas_increment_counter_jit() {
    cas_increment_counter(Box::new(JitBackend::new()));
}

/// `lock xor [WORD], rdx` — the binary lock-ALU path (`lift_binop`, shared by lock
/// and/or/xor/add/sub). Each vcpu XORs the same mask `ITERS` times; `THREADS * ITERS` is
/// even, so an atomic composition returns the word to 0. A torn RMW drops an application.
fn binary_lock_alu_atomic(backend: Box<dyn Backend>) {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut top = a.create_label();
    a.mov(rbx, WORD).unwrap();
    a.mov(rcx, ITERS).unwrap();
    a.mov(rdx, 0x5555_5555_5555_5555u64).unwrap();
    a.set_label(&mut top).unwrap();
    a.lock().xor(qword_ptr(rbx), rdx).unwrap();
    a.dec(rcx).unwrap();
    a.jnz(top).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();
    let vm = vm_with(backend, &code);
    run_all(&vm, |_, _| {});
    assert_eq!(
        read_u64(&vm, WORD),
        0,
        "lock xor lost an update (non-atomic binary RMW)"
    );
}

#[test]
fn binary_lock_alu_atomic_interp() {
    binary_lock_alu_atomic(Box::new(InterpreterBackend));
}

#[test]
fn binary_lock_alu_atomic_jit() {
    binary_lock_alu_atomic(Box::new(JitBackend::new()));
}
