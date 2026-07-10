//! Dynamic linking against **glibc** (spec §1, §4; testing.md §12): the harder
//! rung. Unlike musl's self-contained interpreter, glibc's `ld-linux` opens and
//! **file-backed-mmaps `libc.so.6`** at run time. The loader still does all the
//! linking in guest code; the embedder just serves the library (from a checked-in
//! fixture, via a suffix redirect) and honors file-backed `mmap`. Verified three
//! ways (native == interpreter == JIT).

use x86jit_core::{Backend, GuestCpuFeatures, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;
use x86jit_tests::reference::reference_dyn;

const EXE_BASE: u64 = 0x40_0000;
const INTERP_BASE: u64 = 0x80_0000; // ld-linux (~0x40000) fits below the heap
const HEAP_BASE: u64 = 0x100_0000;
const MMAP_BASE: u64 = 0x180_0000;
const STACK_TOP: u64 = 0x7f0_0000;

fn run_glibc(backend: Box<dyn Backend>, argv: &[&[u8]]) -> (Vec<u8>, Vec<u8>) {
    run_glibc_features(backend, argv, None)
}

fn run_glibc_features(
    backend: Box<dyn Backend>,
    argv: &[&[u8]],
    features: Option<GuestCpuFeatures>,
) -> (Vec<u8>, Vec<u8>) {
    let exe = include_bytes!("../programs/hello_glibc.elf");
    let interp = include_bytes!("../programs/ld-linux-x86-64.so.2");
    let mut g = Guest::new_dynamic(exe, EXE_BASE, interp, INTERP_BASE)
        .flat(0x800_0000) // 128 MiB: libc.so.6 is ~2.4 MiB, plus arenas
        .heap_base(HEAP_BASE)
        .mmap_base(MMAP_BASE)
        .stack_top(STACK_TOP)
        .argv(argv)
        .env(&[b"PATH=/bin"]);
    if let Some(f) = features {
        g = g.features(f);
    }
    let ran = g
        // ld-linux opens libc.so.6 by an absolute (machine-specific) path; serve our
        // fixture for any request ending in it.
        .shim(|s| {
            s.serve_lib(
                b"/libc.so.6".to_vec(),
                concat!(env!("CARGO_MANIFEST_DIR"), "/programs/libc.so.6"),
            )
        })
        .run_full(backend);
    (ran.stdout, ran.stderr)
}

#[test]
fn glibc_hello_native_interp_jit_agree() {
    let reference = reference_dyn(b"hello dynamic\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/hello_glibc.elf"
        ))
        .output()
        .map(|o| o.stdout)
    });

    let argv: &[&[u8]] = &[b"hello_glibc"];
    let (interp, ierr) = run_glibc(Box::new(InterpreterBackend), argv);
    assert_eq!(
        interp,
        reference,
        "interpreter output != reference; guest stderr:\n{}",
        String::from_utf8_lossy(&ierr)
    );
    let (jit, jerr) = run_glibc(Box::new(JitBackend::new()), argv);
    assert_eq!(
        jit,
        reference,
        "JIT output != reference; guest stderr:\n{}",
        String::from_utf8_lossy(&jerr)
    );
}

/// task-168.5 AC#5: run glibc with **AVX-512 advertised** (`GuestCpuFeatures::v4()`).
/// glibc's IFUNC resolver then selects its EVEX string/memory routines, so this drives
/// the AVX-512 lift through real glibc code (startup, printf, memcpy/strlen). The native
/// reference runs on the real (AVX-512) CPU, so output must still match interp and JIT.
#[test]
fn glibc_hello_avx512_interp_jit_agree() {
    let reference = reference_dyn(b"hello dynamic\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/hello_glibc.elf"
        ))
        .output()
        .map(|o| o.stdout)
    });
    let argv: &[&[u8]] = &[b"hello_glibc"];
    let (interp, ierr) = run_glibc_features(
        Box::new(InterpreterBackend),
        argv,
        Some(GuestCpuFeatures::v4()),
    );
    assert_eq!(
        interp,
        reference,
        "interp (v4) != reference; guest stderr:\n{}",
        String::from_utf8_lossy(&ierr)
    );
    let (jit, jerr) = run_glibc_features(
        Box::new(JitBackend::new()),
        argv,
        Some(GuestCpuFeatures::v4()),
    );
    assert_eq!(
        jit,
        reference,
        "JIT (v4) != reference; guest stderr:\n{}",
        String::from_utf8_lossy(&jerr)
    );
}
