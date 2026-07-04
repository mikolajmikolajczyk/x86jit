//! Real-program forcing function (spec §12, testing.md §12): drive an unmodified
//! static-musl **busybox** through the engine and make `busybox sha256sum <file>`
//! produce the same digest three ways (native == interpreter == JIT). busybox is
//! production code we didn't hand-pick, so it surfaces the real gaps.

use std::time::Duration;

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x400_0000; // 64 MiB: binary + heap + mmap arena + stack
const HEAP_BASE: u64 = 0x60_0000; // past busybox's ~0x533000 bss end
const MMAP_BASE: u64 = 0x100_0000;
const STACK_TOP: u64 = 0x3f0_0000;

fn run_busybox(backend: Box<dyn Backend>, argv: &[&[u8]], allow: &[&str]) -> (Vec<u8>, Duration) {
    let image = include_bytes!("../programs/busybox.elf");
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    let entry = load_static_elf(&mut vm, image).expect("load busybox");
    // One big RW region for heap, the mmap arena, and the stack.
    vm.map(
        HEAP_BASE,
        (FLAT - HEAP_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();
    let rsp = setup_stack(&mut vm, STACK_TOP, argv, &[b"PATH=/bin"]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;
    for p in allow {
        shim.allow_read(*p);
    }
    let start = std::time::Instant::now();
    loop {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &mut vm) {
                    break;
                }
            }
            other => panic!("gap at rip={:#x}: {other:?}", cpu.reg(Reg::Rip)),
        }
    }
    (shim.stdout, start.elapsed())
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
    let (interp, _) = run_busybox(Box::new(InterpreterBackend), argv, &[input]);
    let (jit, _) = run_busybox(Box::new(JitBackend::new()), argv, &[input]);
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
    let (interp, _) = run_busybox(Box::new(InterpreterBackend), argv, &[input]);
    let (jit, _) = run_busybox(Box::new(JitBackend::new()), argv, &[input]);
    assert_eq!(interp, reference, "wc: interp != reference");
    assert_eq!(jit, reference, "wc: JIT != reference");
}
