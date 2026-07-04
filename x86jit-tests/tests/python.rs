//! Real-program forcing function, the interpreter summit (spec §12, testing.md
//! §12.5): drive an unmodified static-musl **CPython 3.13** and make `python3 -S
//! -c <script>` produce the same output three ways (native == interpreter ==
//! JIT). CPython is a large, real application — bytecode VM, GC, arbitrary-
//! precision ints, the import machinery — a stringent whole-pipeline exercise.
//!
//! The stdlib is served read-only from a checked-in minimal `pyhome` (just the
//! `encodings` package + a few modules `-S -c` touches; 3.13 freezes the rest);
//! `PYTHONHOME` points the interpreter at it, and `PYTHONDONTWRITEBYTECODE` keeps
//! the read-only tree from provoking `.pyc` writes.

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x800_0000; // 128 MiB
const HEAP_BASE: u64 = 0x200_0000; // past python's ~0x14a0000 bss end
const MMAP_BASE: u64 = 0x280_0000;
const STACK_TOP: u64 = 0x7f0_0000;

const PYHOME: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/programs/pyhome");
const SCRIPT: &str = "print(2 + 2, 'hi' * 3, sum(range(10)), 2 ** 64)";

fn run_python(backend: Box<dyn Backend>) -> Vec<u8> {
    let image = include_bytes!("../programs/python3.elf");
    let mut vm = Vm::with_backend(
        VmConfig { memory_model: MemoryModel::Flat { size: FLAT }, consistency: MemConsistency::Fast },
        backend,
    );
    let entry = load_static_elf(&mut vm, image).expect("load python");
    vm.map(HEAP_BASE, (FLAT - HEAP_BASE) as usize, Prot::RW, RegionKind::Ram).unwrap();

    let home_env = format!("PYTHONHOME={PYHOME}");
    let argv: &[&[u8]] = &[b"python3", b"-S", b"-c", SCRIPT.as_bytes()];
    let envp: &[&[u8]] = &[home_env.as_bytes(), b"PYTHONDONTWRITEBYTECODE=1"];
    let rsp = setup_stack(&mut vm, STACK_TOP, argv, envp).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;
    shim.allow_dir(PYHOME);
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
fn python_script_native_interp_jit_agree() {
    let native = std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/python3.elf"))
        .args(["-S", "-c", SCRIPT])
        .env("PYTHONHOME", PYHOME)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("run native python")
        .stdout;
    assert_eq!(native, b"4 hihihi 45 18446744073709551616\n", "native python output");

    let interp = run_python(Box::new(InterpreterBackend));
    assert_eq!(interp, native, "interpreter output != native");
    let jit = run_python(Box::new(JitBackend::new()));
    assert_eq!(jit, native, "JIT output != native");
}
