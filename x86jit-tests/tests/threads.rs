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
        VmConfig { memory_model: MemoryModel::Flat { size: FLAT }, consistency: MemConsistency::Fast },
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

#[test]
fn parallel_squares_interp() {
    parallel_squares(Box::new(InterpreterBackend));
}

#[test]
fn parallel_squares_jit() {
    parallel_squares(Box::new(JitBackend::new()));
}
