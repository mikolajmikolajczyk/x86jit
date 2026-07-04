//! Dynamic linking (spec §1, §4; testing.md §12): load a real `ET_DYN` PIE and its
//! interpreter (`ld-musl`), hand the interpreter a full auxv, and let it relocate
//! and start the program — all in guest code. The engine never links (§1); the
//! loader lives in `x86jit-elf` and the mmap/mprotect passthrough in the shim, so
//! this exercises the *embedder* path, not the core. Verified three ways
//! (native == interpreter == JIT).

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_dynamic_elf, setup_stack_dyn};
use x86jit_tests::reference::reference_dyn;
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x400_0000; // 64 MiB
const EXE_BASE: u64 = 0x40_0000;
const INTERP_BASE: u64 = 0x80_0000; // ld-musl (~0xc0000) fits below the heap
const HEAP_BASE: u64 = 0x100_0000;
const MMAP_BASE: u64 = 0x180_0000;
const STACK_TOP: u64 = 0x3f0_0000;

fn run_dynamic(backend: Box<dyn Backend>, argv: &[&[u8]]) -> Vec<u8> {
    let exe = include_bytes!("../programs/hello_dyn.elf");
    let interp = include_bytes!("../programs/ld-musl-x86_64.so.1");

    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    let img = load_dynamic_elf(&mut vm, exe, EXE_BASE, interp, INTERP_BASE).expect("load dynamic");
    vm.map(
        HEAP_BASE,
        (FLAT - HEAP_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();
    let rsp = setup_stack_dyn(&mut vm, STACK_TOP, argv, &[b"PATH=/bin"], &img).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, img.entry); // start in the interpreter
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;
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
    shim.stdout
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
