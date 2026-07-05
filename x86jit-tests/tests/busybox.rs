//! Real-program forcing function (spec §12, testing.md §12): drive an unmodified
//! static-musl **busybox** through the engine and make `busybox sha256sum <file>`
//! produce the same digest three ways (native == interpreter == JIT). busybox is
//! production code we didn't hand-pick, so it surfaces the real gaps.

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;
use x86jit_tests::reference::reference;

const FLAT: u64 = 0x400_0000; // 64 MiB: binary + heap + mmap arena + stack
const HEAP_BASE: u64 = 0x60_0000; // past busybox's ~0x533000 bss end
const MMAP_BASE: u64 = 0x100_0000;
const STACK_TOP: u64 = 0x3f0_0000;

fn run_busybox(backend: Box<dyn Backend>, argv: &[&[u8]], allow: &[&str]) -> Vec<u8> {
    Guest::new_static(include_bytes!("../programs/busybox.elf"))
        .flat(FLAT)
        .heap_base(HEAP_BASE)
        .mmap_base(MMAP_BASE)
        .stack_top(STACK_TOP)
        .argv(argv)
        .env(&[b"PATH=/bin"])
        .shim(move |s| {
            for p in allow {
                s.allow_read(*p);
            }
        })
        .run(backend)
}

fn run_busybox_stdin(backend: Box<dyn Backend>, argv: &[&[u8]], stdin: &[u8]) -> Vec<u8> {
    Guest::new_static(include_bytes!("../programs/busybox.elf"))
        .flat(FLAT)
        .heap_base(HEAP_BASE)
        .mmap_base(MMAP_BASE)
        .stack_top(STACK_TOP)
        .argv(argv)
        .env(&[b"PATH=/bin"])
        .stdin(stdin)
        .run(backend)
}

/// `busybox awk` float `printf` formats via musl's 80-bit `long double`; true-80-bit
/// x87 makes the last-digit rounding match hardware (before, `%.6f` of sqrt(2) gave
/// `1.414213` vs native `1.414214`). Three ways.
#[test]
fn busybox_awk_float_printf_native_interp_jit_agree() {
    let prog: &[u8] = b"BEGIN{ printf \"%.6f|%.10f|%.17g\\n\", sqrt(2), 1.0/3.0, atan2(1,1)*4 }";
    let reference = reference(b"1.414214|0.3333333333|3.1415926535897931\n", || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/busybox.elf"))
            .args([
                "awk",
                "BEGIN{ printf \"%.6f|%.10f|%.17g\\n\", sqrt(2), 1.0/3.0, atan2(1,1)*4 }",
            ])
            .output()
            .expect("run native busybox awk")
            .stdout
    });
    let argv: &[&[u8]] = &[b"busybox", b"awk", prog];
    let interp = run_busybox(Box::new(InterpreterBackend), argv, &[]);
    let jit = run_busybox(Box::new(JitBackend::new()), argv, &[]);
    assert_eq!(interp, reference, "awk float printf: interp != reference");
    assert_eq!(jit, reference, "awk float printf: JIT != reference");
}

/// `busybox sort -n` reads stdin and exercises **SHLD** (double-precision shift) in
/// its number comparison — the applet that surfaced the missing instruction.
#[test]
fn busybox_sort_numeric_native_interp_jit_agree() {
    let input = b"3\n1\n20\n2\n100\n7\n";
    let reference = reference(b"1\n2\n3\n7\n20\n100\n", || {
        use std::io::Write;
        let mut child = std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/busybox.elf"
        ))
        .args(["sort", "-n"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn native busybox sort");
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().expect("native busybox sort").stdout
    });

    let argv: &[&[u8]] = &[b"busybox", b"sort", b"-n"];
    let interp = run_busybox_stdin(Box::new(InterpreterBackend), argv, input);
    let jit = run_busybox_stdin(Box::new(JitBackend::new()), argv, input);
    assert_eq!(interp, reference, "sort -n: interp != reference");
    assert_eq!(jit, reference, "sort -n: JIT != reference");
}

#[test]
fn busybox_sha256sum_native_interp_jit_agree() {
    let input = concat!(env!("CARGO_MANIFEST_DIR"), "/programs/busybox_input.txt");
    // `sha256sum` prints "<hex>  <path>\n"; the digest is fixed by the checked-in
    // input, the path is this build's absolute fixture path.
    const DIGEST: &[u8] = b"b47cc0f104b62d4c7c30bcd68fd8e67613e287dc4ad8c310ef10cbadea9c4380";
    let expected = [DIGEST, b"  ", input.as_bytes(), b"\n"].concat();
    let reference = reference(&expected, || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/busybox.elf"))
            .args(["sha256sum", input])
            .output()
            .expect("run native busybox")
            .stdout
    });

    let argv: &[&[u8]] = &[b"busybox", b"sha256sum", input.as_bytes()];
    let interp = run_busybox(Box::new(InterpreterBackend), argv, &[input]);
    let jit = run_busybox(Box::new(JitBackend::new()), argv, &[input]);
    assert_eq!(interp, reference, "interpreter digest != reference");
    assert_eq!(jit, reference, "JIT digest != reference");
}

/// Generality probe: a second applet (`wc -c`) over the same engine, three ways.
#[test]
fn busybox_wc_native_interp_jit_agree() {
    let input = concat!(env!("CARGO_MANIFEST_DIR"), "/programs/busybox_input.txt");
    // `wc -c` prints "<count> <path>\n"; the count is fixed by the input's size.
    let expected = [b"45 ", input.as_bytes(), b"\n"].concat();
    let reference = reference(&expected, || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/busybox.elf"))
            .args(["wc", "-c", input])
            .output()
            .expect("run native busybox wc")
            .stdout
    });

    let argv: &[&[u8]] = &[b"busybox", b"wc", b"-c", input.as_bytes()];
    let interp = run_busybox(Box::new(InterpreterBackend), argv, &[input]);
    let jit = run_busybox(Box::new(JitBackend::new()), argv, &[input]);
    assert_eq!(interp, reference, "wc: interp != reference");
    assert_eq!(jit, reference, "wc: JIT != reference");
}
