//! Multithreading (M7, spec §11): many `Vcpu`s run on separate host threads over
//! one shared `Arc<Vm>` — the KVM-style split (§2). Each vcpu owns its `CpuState`
//! and `run()` loop; guest memory and the translation cache are shared. This
//! exercises M7-T1 (threads over `Arc<Vm>`), M7-T2 (concurrent cache fill — every
//! thread runs the same code address, so one compiles and the rest hit), and
//! M7-T3 (the `Send + Sync` chain the M3/M4 `CompiledPtr` wrapper was built for).
//!
//! Cross-thread *memory ordering* on weak hosts (the TSO barrier tiers, M7-T4) is
//! not exercised here: this runs on an x86 host (native TSO, all tiers identical)
//! and needs an ARM host plus atomic RMW lifting to demonstrate — see
//! wiki/tasks/m7-multithreading-tso.md.

use std::sync::Arc;
use std::thread;

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, CachedBlock, CompiledPtr, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot,
    Reg, RegionKind, Vcpu, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x10_0000;
const CODE: u64 = 0x1000;
const RESULTS: u64 = 0x8000; // one u64 slot per thread
const THREADS: u64 = 8;

/// M7-T3: the shared types must be `Send + Sync` for the threaded cache to be
/// sound. A compile-time check — if the M4 `CompiledPtr` wrapper had been skipped,
/// this wouldn't build (the M7 trap).
#[test]
fn shared_types_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Vm>();
    assert_send_sync::<CachedBlock>();
    assert_send_sync::<CompiledPtr>();
    fn assert_send<T: Send>() {}
    assert_send::<Vcpu>(); // moved into each thread
}

const ITERS: u64 = 2000;

/// Spawn `THREADS` vcpus over one `Arc<Vm>`. Each runs the same guest code with
/// its thread id in RDI, accumulating `results[id] = id * ITERS` in a hot loop.
/// Distinct slots → deterministic result; the loop re-runs one cached block
/// thousands of times, so the shared cache and memory are hammered concurrently.
fn parallel_squares(backend: Box<dyn Backend>) {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();

    // rax = id * ITERS via a loop; results[id] = rax.
    let mut a = CodeAssembler::new(64).unwrap();
    let mut top = a.create_label();
    a.mov(rbx, RESULTS).unwrap();
    a.mov(rcx, ITERS).unwrap();
    a.xor(rax, rax).unwrap();
    a.set_label(&mut top).unwrap();
    a.add(rax, rdi).unwrap();
    a.dec(rcx).unwrap();
    a.jnz(top).unwrap();
    a.mov(qword_ptr(rbx + rdi * 8), rax).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    let vm = Arc::new(vm);
    let handles: Vec<_> = (0..THREADS)
        .map(|id| {
            let vm = Arc::clone(&vm);
            thread::spawn(move || {
                let mut cpu = vm.new_vcpu();
                cpu.set_reg(Reg::Rip, CODE);
                cpu.set_reg(Reg::Rdi, id);
                match cpu.run(&vm, None) {
                    Exit::Hlt => {}
                    other => panic!("thread {id} unexpected exit: {other:?}"),
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    for id in 0..THREADS {
        let mut buf = [0u8; 8];
        vm.read_bytes(RESULTS + id * 8, &mut buf).unwrap();
        assert_eq!(u64::from_le_bytes(buf), id * ITERS, "results[{id}]");
    }
    // The loop body runs ITERS times per thread but is lifted at most once per
    // (thread, block) — reused thereafter via a cache hit (interp) or a chained
    // link (JIT). So total lifts stay O(threads × blocks), never O(threads ×
    // iters): proof the shared cache is doing its job. The program has 3 blocks.
    assert!(
        vm.cache.misses() <= THREADS * 8,
        "expected bounded lifts, got {} (reuse failed)",
        vm.cache.misses(),
    );
}

const COUNTER: u64 = 0x9000;
const INCS: u64 = 20_000;

/// Contended atomic counter: every thread does `lock inc [COUNTER]` `INCS` times
/// over one `Arc<Vm>`. The result is deterministic (`THREADS * INCS`) *only* if the
/// increment is genuinely atomic — a non-atomic RMW would lose updates under the
/// race. Proves locked ops lower to real host atomics on both backends (M7-T4b).
fn contended_counter(backend: Box<dyn Backend>) {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();

    let mut a = CodeAssembler::new(64).unwrap();
    let mut top = a.create_label();
    a.mov(rbx, COUNTER).unwrap();
    a.mov(rcx, INCS).unwrap();
    a.set_label(&mut top).unwrap();
    a.lock().inc(qword_ptr(rbx)).unwrap();
    a.dec(rcx).unwrap();
    a.jnz(top).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    let vm = Arc::new(vm);
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let vm = Arc::clone(&vm);
            thread::spawn(move || {
                let mut cpu = vm.new_vcpu();
                cpu.set_reg(Reg::Rip, CODE);
                match cpu.run(&vm, None) {
                    Exit::Hlt => {}
                    other => panic!("unexpected exit: {other:?}"),
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let mut buf = [0u8; 8];
    vm.read_bytes(COUNTER, &mut buf).unwrap();
    assert_eq!(
        u64::from_le_bytes(buf),
        THREADS * INCS,
        "atomic counter lost updates"
    );
}

const TOGGLE: u64 = 0xA000;

/// Contended `lock not` / `lock neg`: `THREADS` vcpus each apply the op `INCS` times
/// to one shared word. Both are self-inverse (`not∘not = id`, `neg∘neg = id`) and
/// `THREADS * INCS` is even, so composing an even number of atomic applications
/// returns the word to its initial value — regardless of interleaving. A torn RMW
/// (two vcpus reading the same value, both writing) drops one application, flipping
/// the parity to the wrong final value. Guards #7 (LOCK dropped for NEG/NOT).
fn contended_selfinverse(
    backend: Box<dyn Backend>,
    emit: impl Fn(&mut CodeAssembler),
    init: u64,
    expected: u64,
) {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(TOGGLE, &init.to_le_bytes()).unwrap();

    let mut a = CodeAssembler::new(64).unwrap();
    let mut top = a.create_label();
    a.mov(rbx, TOGGLE).unwrap();
    a.mov(rcx, INCS).unwrap();
    a.set_label(&mut top).unwrap();
    emit(&mut a); // the lock-prefixed neg/not on [rbx]
    a.dec(rcx).unwrap();
    a.jnz(top).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    let vm = Arc::new(vm);
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let vm = Arc::clone(&vm);
            thread::spawn(move || {
                let mut cpu = vm.new_vcpu();
                cpu.set_reg(Reg::Rip, CODE);
                match cpu.run(&vm, None) {
                    Exit::Hlt => {}
                    other => panic!("unexpected exit: {other:?}"),
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let mut buf = [0u8; 8];
    vm.read_bytes(TOGGLE, &mut buf).unwrap();
    assert_eq!(
        u64::from_le_bytes(buf),
        expected,
        "self-inverse lock op lost an update (torn RMW)"
    );
}

#[test]
fn contended_lock_not_interp() {
    contended_selfinverse(
        Box::new(InterpreterBackend),
        |a| {
            a.lock().not(qword_ptr(rbx)).unwrap();
        },
        0,
        0,
    );
}

#[test]
fn contended_lock_not_jit() {
    contended_selfinverse(
        Box::new(JitBackend::new()),
        |a| {
            a.lock().not(qword_ptr(rbx)).unwrap();
        },
        0,
        0,
    );
}

#[test]
fn contended_lock_neg_interp() {
    contended_selfinverse(
        Box::new(InterpreterBackend),
        |a| {
            a.lock().neg(qword_ptr(rbx)).unwrap();
        },
        1,
        1,
    );
}

#[test]
fn contended_lock_neg_jit() {
    contended_selfinverse(
        Box::new(JitBackend::new()),
        |a| {
            a.lock().neg(qword_ptr(rbx)).unwrap();
        },
        1,
        1,
    );
}

#[test]
fn contended_counter_interp() {
    contended_counter(Box::new(InterpreterBackend));
}

#[test]
fn contended_counter_jit() {
    contended_counter(Box::new(JitBackend::new()));
}

#[test]
fn parallel_squares_interp() {
    parallel_squares(Box::new(InterpreterBackend));
}

#[test]
fn parallel_squares_jit() {
    parallel_squares(Box::new(JitBackend::new()));
}
