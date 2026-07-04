//! Whole-program end-to-end (M2, testing.md §11): load a freestanding static ELF
//! that issues `write`/`exit` via raw `syscall`, run it under the interpreter
//! through the syscall shim, and assert its observable output — the psychological
//! milestone. This exercises the full pipeline: loader → lift → dispatcher →
//! interpreter → syscall shim.
//!
//! The fixture (`programs/hello_static.s`) is nolibc on purpose (§16): a static
//! glibc binary would run SSE2 `memcpy`/`strlen` in `__libc_start_main` before
//! printing anything, secretly requiring SIMD (M8).

use std::time::{Duration, Instant};

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;

const FLAT_SIZE: u64 = 0x80_0000; // 0x400000-based image + heap + stack
const HEAP_BASE: u64 = 0x50_0000;
const HEAP_SIZE: u64 = 0x10_0000;
const STACK_BASE: u64 = 0x68_0000;
const STACK_TOP: u64 = 0x70_0000;

#[test]
fn hello_static_elf_prints_hello() {
    let image = include_bytes!("../programs/hello_static.elf");

    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: FLAT_SIZE },
        consistency: MemConsistency::Fast,
    });
    let entry = load_static_elf(&mut vm, image).expect("load static elf");
    vm.map(
        STACK_BASE,
        (STACK_TOP - STACK_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();

    // Full System V initial stack (argc/argv/envp/auxv). This freestanding binary
    // ignores it, but the setup exercises the path real `_start`s depend on.
    let rsp = setup_stack(
        &mut vm,
        STACK_TOP,
        &[b"hello_static.elf"],
        &[b"PATH=/usr/bin"],
    )
    .unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    for _ in 0..100 {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &mut vm) {
                    break;
                }
            }
            other => panic!("unexpected exit before program finished: {other:?}"),
        }
    }

    assert_eq!(shim.stdout, b"hello\n", "emulated program's stdout");
    assert_eq!(shim.exit_code, Some(0), "exit code");
}

/// Proves `setup_stack` semantically: this ELF reads `argv[1]` off the stack,
/// `write`s it, and `exit`s with `argc`. If the SysV layout were wrong the guest
/// would print garbage / crash rather than echo the argument.
#[test]
fn argv_is_read_from_the_stack() {
    let image = include_bytes!("../programs/echo_argv.elf");

    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: FLAT_SIZE },
        consistency: MemConsistency::Fast,
    });
    let entry = load_static_elf(&mut vm, image).expect("load static elf");
    vm.map(
        STACK_BASE,
        (STACK_TOP - STACK_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();

    // argc = 2 (prog name + "WORLD").
    let rsp = setup_stack(&mut vm, STACK_TOP, &[b"echo_argv", b"WORLD"], &[]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    for _ in 0..1000 {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &mut vm) {
                    break;
                }
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }

    assert_eq!(shim.stdout, b"WORLD", "guest echoed argv[1] from the stack");
    assert_eq!(shim.exit_code, Some(2), "guest exited with argc");
}

/// Load `image`, run it on `backend` through the syscall shim, and return the
/// captured stdout plus the wall-clock of the run.
fn run_program(
    image: &[u8],
    backend: Box<dyn Backend>,
    argv: &[&[u8]],
    allow_read: &[&str],
) -> (Vec<u8>, Duration) {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT_SIZE },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    let entry = load_static_elf(&mut vm, image).expect("load elf");
    vm.map(HEAP_BASE, HEAP_SIZE as usize, Prot::RW, RegionKind::Ram)
        .unwrap();
    vm.map(
        STACK_BASE,
        (STACK_TOP - STACK_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();
    let rsp = setup_stack(&mut vm, STACK_TOP, argv, &[]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = HEAP_BASE + HEAP_SIZE;
    for path in allow_read {
        shim.allow_read(*path);
    }
    let start = Instant::now();
    loop {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &mut vm) {
                    break;
                }
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    (shim.stdout, start.elapsed())
}

/// SHA-256 whole-program: a real scalar workload (5000 hash iterations) run three
/// ways — native, interpreter, JIT — all must agree (testing.md §12), and the run
/// is a realistic block-mix benchmark of the JIT vs the interpreter (§8.3).
#[test]
fn sha256_native_interp_jit_agree() {
    let image = include_bytes!("../programs/sha256.elf");
    // Deterministic 32-byte raw digest of the program's fixed input.
    let expected = b"\xe7\x2b\x9a\x3d\x7e\x6f\x05\x3e\x6b\xbd\x38\x8c\xa2\x8b\x15\x49\
                     \xf0\x21\x25\xf7\x62\x94\x4a\x9b\x81\x11\x96\x97\xdd\xd1\x7d\x94";
    let native = reference(expected, || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/sha256.elf"))
            .output()
            .expect("run native sha256")
            .stdout
    });

    let (interp, ti) = run_program(image, Box::new(InterpreterBackend), &[b"sha256"], &[]);
    let (jit, tj) = run_program(image, Box::new(JitBackend::new()), &[b"sha256"], &[]);

    assert_eq!(interp, native, "interpreter digest != reference");
    assert_eq!(jit, native, "JIT digest != reference");

    eprintln!(
        "sha256 (5000 iters): interp {:.1} ms, jit {:.1} ms, speedup {:.1}x",
        ti.as_secs_f64() * 1e3,
        tj.as_secs_f64() * 1e3,
        ti.as_secs_f64() / tj.as_secs_f64()
    );
}
/// A real static musl libc binary runs end-to-end: `_start` → `__libc_start_main`
/// → `main` → `write`/`exit`, through the syscall shim (brk / arch_prctl-TLS /
/// set_tid_address). Verified three ways — native, interpreter, JIT (testing.md
/// §12) — the first real libc program on the engine.
#[test]
fn musl_hello_native_interp_jit_agree() {
    let image = include_bytes!("../programs/hello_musl.elf");
    let native = reference(b"hello musl\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/hello_musl.elf"
        ))
        .output()
        .expect("run native musl")
        .stdout
    });

    let (interp, _) = run_program(image, Box::new(InterpreterBackend), &[b"hello_musl"], &[]);
    let (jit, _) = run_program(image, Box::new(JitBackend::new()), &[b"hello_musl"], &[]);
    assert_eq!(interp, native, "interpreter output != reference");
    assert_eq!(jit, native, "JIT output != reference");
}

/// Scalar SSE2 double arithmetic end-to-end: a freestanding Newton's-method
/// `sqrt(2)` (mulsd/subsd/divsd/addsd/movsd/movapd, then `cvttsd2si` to print the
/// scaled integer). Deterministic under IEEE-754, run three ways — native,
/// interpreter, JIT — all must agree (testing.md §12). The first floating-point
/// program on the engine.
#[test]
fn newton_sqrt2_native_interp_jit_agree() {
    let image = include_bytes!("../programs/newton.elf");
    let native = reference(b"1414213562\n", || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/newton.elf"))
            .output()
            .expect("run native newton")
            .stdout
    });

    let (interp, _) = run_program(image, Box::new(InterpreterBackend), &[b"newton"], &[]);
    let (jit, _) = run_program(image, Box::new(JitBackend::new()), &[b"newton"], &[]);
    assert_eq!(interp, native, "interpreter output != reference");
    assert_eq!(jit, native, "JIT output != reference");
}

/// Syscall passthrough (testing.md §12): a static musl `sha256sum` opens a real
/// file (`open`/`read`/`close` forwarded to the host kernel through the shim's
/// read-only allowlist), hashes it, and prints the hex digest. Run three ways —
/// native, interpreter, JIT — all must agree. Proves the engine drives a real
/// libc program that does genuine host file I/O, not just stdout.
#[test]
fn sha256sum_passthrough_native_interp_jit_agree() {
    let image = include_bytes!("../programs/sha256sum.elf");
    let input = concat!(env!("CARGO_MANIFEST_DIR"), "/programs/sha256sum_input.txt");

    // Deterministic digest line ("<64 hex>\n") of the checked-in input file.
    let expected = b"b47cc0f104b62d4c7c30bcd68fd8e67613e287dc4ad8c310ef10cbadea9c4380\n";
    let native = reference(expected, || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/sha256sum.elf"
        ))
        .arg(input)
        .output()
        .expect("run native sha256sum")
        .stdout
    });

    let argv: &[&[u8]] = &[b"sha256sum", input.as_bytes()];
    let (interp, _) = run_program(image, Box::new(InterpreterBackend), argv, &[input]);
    let (jit, _) = run_program(image, Box::new(JitBackend::new()), argv, &[input]);
    assert_eq!(interp, native, "interpreter digest != reference");
    assert_eq!(jit, native, "JIT digest != reference");
}
