//! go-caddy P2.2 de-risk: run the single-process corpus through the **threaded
//! driver** (`x86jit_linux::thread::run_threaded`) on one worker thread, and assert
//! it produces the same stdout as the inline `Guest::run` loop — on both backends.
//!
//! This validates the P2.0 Send refactor, the `&Vm` migration, and the
//! `Arc<Mutex<LinuxShim>>`-over-`Arc<Vm>` lock discipline against everything that
//! already works, BEFORE any concurrency (futex/clone) is added. A regression here
//! means the threading plumbing broke a plain sequential program.

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;

const FLAT_SIZE: u64 = 0x80_0000;
const HEAP_BASE: u64 = 0x50_0000;
const HEAP_SIZE: u64 = 0x10_0000;
const STACK_TOP: u64 = 0x70_0000;

fn guest(image: &'static [u8], argv: &'static [&'static [u8]]) -> Guest<'static> {
    Guest::new_static(image)
        .flat(FLAT_SIZE)
        .heap_base(HEAP_BASE)
        .brk_limit(HEAP_BASE + HEAP_SIZE)
        .stack_top(STACK_TOP)
        .argv(argv)
}

/// Same program, same backend, two drivers: inline loop vs threaded driver → same
/// stdout. Runs for both the interpreter and the JIT.
fn both_drivers_agree(image: &'static [u8], argv: &'static [&'static [u8]]) {
    for make in [
        (|| Box::new(InterpreterBackend) as Box<dyn Backend>) as fn() -> Box<dyn Backend>,
        || Box::new(JitBackend::new()) as Box<dyn Backend>,
    ] {
        let inline = guest(image, argv).run(make());
        let threaded = guest(image, argv).run_threaded(make());
        assert_eq!(
            threaded, inline,
            "threaded-driver stdout != inline-loop stdout"
        );
    }
}

#[test]
fn hello_static_threaded_matches_inline() {
    both_drivers_agree(include_bytes!("../programs/hello_static.elf"), &[b"hello"]);
}

#[test]
fn musl_hello_threaded_matches_inline() {
    both_drivers_agree(
        include_bytes!("../programs/hello_musl.elf"),
        &[b"hello_musl"],
    );
}

#[test]
fn newton_threaded_matches_inline() {
    both_drivers_agree(include_bytes!("../programs/newton.elf"), &[b"newton"]);
}

#[test]
fn sha256_threaded_matches_inline() {
    both_drivers_agree(include_bytes!("../programs/sha256.elf"), &[b"sha256"]);
}
