//! Jaguar-class ISA (SSSE3 / SSE4.1 / SSE4.2 / POPCNT / CRC32): cpuid advertises
//! the x86-64-v2 line, so a program built `-march=x86-64-v2` uses those
//! instructions directly. Run three ways (native == interpreter == JIT).
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;
const FLAT: u64 = 0x80_0000;
const HEAP: u64 = 0x50_0000;
const MMAP: u64 = 0x60_0000;
const STK: u64 = 0x70_0000;
fn run(b: Box<dyn Backend>) -> Vec<u8> {
    let img = include_bytes!("../programs/v2.elf");
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        b,
    );
    let e = load_static_elf(&mut vm, img).unwrap();
    vm.map(HEAP, (FLAT - HEAP) as usize, Prot::RW, RegionKind::Ram)
        .unwrap();
    let rsp = setup_stack(&mut vm, STK, &[b"v2"], &[]).unwrap();
    let mut c = vm.new_vcpu();
    c.set_reg(Reg::Rip, e);
    c.set_reg(Reg::Rsp, rsp);
    let mut sh = LinuxShim::new();
    sh.brk = HEAP;
    sh.brk_limit = MMAP;
    sh.mmap_base = MMAP;
    sh.mmap_limit = STK - 0x10000;
    loop {
        match c.run(&vm, None) {
            Exit::Syscall => {
                if sh.handle(&mut c, &mut vm) {
                    break;
                }
            }
            o => panic!("gap rip={:#x}: {o:?}", c.reg(Reg::Rip)),
        }
    }
    sh.stdout
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
