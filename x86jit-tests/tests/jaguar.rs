//! Jaguar-class ISA (SSSE3 / SSE4.1 / SSE4.2 / POPCNT / CRC32): cpuid advertises
//! the x86-64-v2 line, so a program built `-march=x86-64-v2` uses those
//! instructions directly. Run three ways (native == interpreter == JIT).
use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;
use x86jit_tests::reference::reference;
const FLAT: u64 = 0x80_0000;
const HEAP: u64 = 0x50_0000;
const MMAP: u64 = 0x60_0000;
const STK: u64 = 0x70_0000;
fn run(b: Box<dyn Backend>) -> Vec<u8> {
    Guest::new_static(include_bytes!("../programs/v2.elf"))
        .flat(FLAT)
        .heap_base(HEAP)
        .mmap_base(MMAP)
        .mmap_limit(STK - 0x10000)
        .stack_top(STK)
        .argv(&[b"v2"])
        .run(b)
}
#[test]
fn v2_native_interp_jit_agree() {
    let reference = reference(b"872\n", || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/v2.elf"))
            .output()
            .unwrap()
            .stdout
    });
    assert_eq!(run(Box::new(InterpreterBackend)), reference, "interp");
    assert_eq!(run(Box::new(JitBackend::new())), reference, "jit");
}
