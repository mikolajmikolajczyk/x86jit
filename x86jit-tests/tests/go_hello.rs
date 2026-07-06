//! go-caddy Go acceptance. P1b: the Go build-note heuristic that selects the Reserved
//! span + threaded driver. P3: a static Go hello runs three ways (native / interp /
//! JIT) through the production shim — real `clone`, futex, the signal/advice stubs Go's
//! `minit` needs (madvise / sigaltstack / rt_sigaction / rt_sigprocmask / prlimit64),
//! and the host-monotonic clock, over a 1 TiB Reserved NORESERVE span.
//!
//! Weak-host (ARM) memory ordering under real concurrency is out of scope (M7-T4); the
//! native leg is skipped on non-x86 hosts (like the other three-way tests).

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_elf::has_go_build_note;
use x86jit_tests::guest::Guest;

// The Go/Reserved layout the runner uses (x86jit-run P1b): a 1 TiB sparse span with a
// low stack + brk and the mmap arena placed high, where Go grows its heap.
const GO_SPAN: u64 = 1 << 40; // 1 TiB
const HEAP_BASE: u64 = 0x100_0000; // 16 MiB — above the Go image
const BRK_LIMIT: u64 = 0x180_0000; // 24 MiB
const STACK_TOP: u64 = 0x8000_0000; // 2 GiB
const MMAP_BASE: u64 = 0x1_0000_0000; // 4 GiB
const MMAP_LIMIT: u64 = MMAP_BASE + (512 << 30); // 516 GiB

fn run_go(backend: Box<dyn Backend>) -> Vec<u8> {
    Guest::new_static(include_bytes!("../programs/hello_go.elf"))
        .reserved(GO_SPAN)
        .heap_base(HEAP_BASE)
        .brk_limit(BRK_LIMIT)
        .mmap_base(MMAP_BASE)
        .mmap_limit(MMAP_LIMIT)
        .stack_top(STACK_TOP)
        .argv(&[b"hello_go"])
        .run_threaded(backend)
}

fn reference() -> Vec<u8> {
    x86jit_tests::reference::reference(b"hello from go stdout\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/hello_go.elf"
        ))
        .output()
        .expect("run native go")
        .stdout
    })
}

/// P3 DoD-1: static Go hello prints its line under the interpreter and the JIT, matching
/// the native reference — the whole Reserved-span + threads + signal-stub stack end to
/// end on a real Go binary.
#[test]
fn go_hello_shim_interp() {
    assert_eq!(
        run_go(Box::new(InterpreterBackend)),
        reference(),
        "interpreter"
    );
}

#[test]
fn go_hello_shim_jit() {
    assert_eq!(run_go(Box::new(JitBackend::new())), reference(), "JIT");
}

/// P1b: a Go entrypoint is recognized by its `PT_NOTE` build note (which survives
/// `strip`/`-s -w`), and a non-Go static ELF is not — this is what the runner keys the
/// Reserved-span + threaded-driver choice off.
#[test]
fn go_build_note_detected_only_for_go() {
    let go = include_bytes!("../programs/hello_go.elf");
    let not_go = include_bytes!("../programs/hello_static.elf");
    assert!(has_go_build_note(go), "Go binary carries the Go build note");
    assert!(
        !has_go_build_note(not_go),
        "a musl static ELF has no Go build note"
    );
    assert!(!has_go_build_note(b"not an elf"), "garbage isn't Go");
}
