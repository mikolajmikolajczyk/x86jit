//! Dynamic linking (spec §1, §4; testing.md §12): load a real `ET_DYN` PIE and its
//! interpreter (`ld-musl`), hand the interpreter a full auxv, and let it relocate
//! and start the program — all in guest code. The engine never links (§1); the
//! loader lives in `x86jit-elf` and the mmap/mprotect passthrough in the shim, so
//! this exercises the *embedder* path, not the core. Verified three ways
//! (native == interpreter == JIT).

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;
use x86jit_tests::reference::reference_dyn;

const EXE_BASE: u64 = 0x40_0000;
const INTERP_BASE: u64 = 0x80_0000; // ld-musl (~0xc0000) fits below the heap
const HEAP_BASE: u64 = 0x100_0000;
const MMAP_BASE: u64 = 0x180_0000;
const STACK_TOP: u64 = 0x3f0_0000;

fn run_dynamic(backend: Box<dyn Backend>, argv: &[&[u8]]) -> Vec<u8> {
    let exe = include_bytes!("../programs/hello_dyn.elf");
    let interp = include_bytes!("../programs/ld-musl-x86_64.so.1");
    Guest::new_dynamic(exe, EXE_BASE, interp, INTERP_BASE)
        .flat(0x400_0000) // 64 MiB
        .heap_base(HEAP_BASE)
        .mmap_base(MMAP_BASE)
        .stack_top(STACK_TOP)
        .argv(argv)
        .env(&[b"PATH=/bin"])
        .run(backend)
}

#[test]
fn dynamic_hello_native_interp_jit_agree() {
    let reference = reference_dyn(b"hello dynamic\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/hello_dyn.elf"
        ))
        .output()
        .map(|o| o.stdout)
    });

    let argv: &[&[u8]] = &[b"hello_dyn"];
    let interp = run_dynamic(Box::new(InterpreterBackend), argv);
    let jit = run_dynamic(Box::new(JitBackend::new()), argv);
    assert_eq!(interp, reference, "interpreter output != reference");
    assert_eq!(jit, reference, "JIT output != reference");
}
