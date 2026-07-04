//! Dynamic linking against **glibc** (spec §1, §4; testing.md §12): the harder
//! rung. Unlike musl's self-contained interpreter, glibc's `ld-linux` opens and
//! **file-backed-mmaps `libc.so.6`** at run time. The loader still does all the
//! linking in guest code; the embedder just serves the library (from a checked-in
//! fixture, via a suffix redirect) and honors file-backed `mmap`. Verified three
//! ways (native == interpreter == JIT).

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_dynamic_elf, setup_stack_dyn};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x800_0000; // 128 MiB: libc.so.6 is ~2.4 MiB, plus arenas
const EXE_BASE: u64 = 0x40_0000;
const INTERP_BASE: u64 = 0x80_0000; // ld-linux (~0x40000) fits below the heap
const HEAP_BASE: u64 = 0x100_0000;
const MMAP_BASE: u64 = 0x180_0000;
const STACK_TOP: u64 = 0x7f0_0000;

fn run_glibc(backend: Box<dyn Backend>, argv: &[&[u8]]) -> (Vec<u8>, Vec<u8>) {
    let exe = include_bytes!("../programs/hello_glibc.elf");
    let interp = include_bytes!("../programs/ld-linux-x86-64.so.2");

    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    let img =
        load_dynamic_elf(&mut vm, exe, EXE_BASE, interp, INTERP_BASE).expect("load glibc exe");
    vm.map(
        HEAP_BASE,
        (FLAT - HEAP_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();
    let rsp = setup_stack_dyn(&mut vm, STACK_TOP, argv, &[b"PATH=/bin"], &img).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, img.entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;
    // ld-linux opens libc.so.6 by an absolute (machine-specific) path; serve our
    // fixture for any request ending in it.
    shim.serve_lib(
        b"/libc.so.6".to_vec(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/programs/libc.so.6"),
    );
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
    (shim.stdout, shim.stderr)
}

#[test]
fn glibc_hello_native_interp_jit_agree() {
    let reference = reference(b"hello dynamic\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/hello_glibc.elf"
        ))
        .output()
        .expect("run native glibc hello")
        .stdout
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
