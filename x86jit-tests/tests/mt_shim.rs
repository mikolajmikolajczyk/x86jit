//! go-caddy P2.7 (M7-T5, DoD-1): the pthreads acceptance program run through the
//! **production** `LinuxShim` + threaded driver (`x86jit_linux::thread::run_threaded`),
//! not the `mt.rs` toy `handle`. Four guest pthreads each increment a shared counter
//! 100 000 times under a futex-backed mutex; the deterministic result (`400000`) only
//! holds if real `clone(CLONE_VM)` spawning, cross-thread atomics, the futex mutex, and
//! `pthread_join` (clear_tid + futex wake) all work in the real shim — the P2.4/P2.5
//! promotion. Runs on both engines against the native reference.
//!
//! Weak-host (ARM) memory ordering under genuine concurrency is out of scope here
//! (M7-T4, `MemConsistency::Fast`); this asserts x86-host correctness.

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;

// Same layout as mt.rs: main stack below the mmap arena, thread stacks mmap'd above it.
const FLAT: u64 = 0x200_0000; // 32 MiB
const HEAP_BASE: u64 = 0x60_0000;
const STACK_TOP: u64 = 0xf0_0000;
const MMAP_BASE: u64 = 0x100_0000;

fn run(backend: Box<dyn Backend>) -> Vec<u8> {
    Guest::new_static(include_bytes!("../programs/pthreads.elf"))
        .flat(FLAT)
        .heap_base(HEAP_BASE)
        .stack_top(STACK_TOP)
        .mmap_base(MMAP_BASE)
        .mmap_limit(FLAT - 0x1000)
        .argv(&[b"pthreads"])
        .run_threaded(backend)
}

fn reference() -> Vec<u8> {
    x86jit_tests::reference::reference(b"400000\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/pthreads.elf"
        ))
        .output()
        .expect("run native pthreads")
        .stdout
    })
}

#[test]
fn pthreads_counter_shim_interp() {
    assert_eq!(
        run(Box::new(InterpreterBackend)),
        reference(),
        "interpreter"
    );
}

#[test]
fn pthreads_counter_shim_jit() {
    assert_eq!(run(Box::new(JitBackend::new())), reference(), "JIT");
}
