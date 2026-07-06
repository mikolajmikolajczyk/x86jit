//! Wiring in the Cranelift JIT backend and checking it against the interpreter.
//! The core's differential invariant is that both backends compute the same guest
//! state (spec §4.1); this is how an embedder opts into the JIT — inject a
//! `JitBackend` instead of the default interpreter.
//!
//! Run with: `cargo run -p x86jit-cranelift --example jit_vs_interp`

use x86jit_core::{
    Backend, Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const RAM: u64 = 0x1_0000;
const ENTRY: u64 = 0x1000;

// mov eax, 7   ; imul eax, eax, 6  ; sub eax, 2  ; hlt   -> eax = 40
#[rustfmt::skip]
const CODE: &[u8] = &[
    0xB8, 0x07, 0x00, 0x00, 0x00,       // mov  eax, 7
    0x6B, 0xC0, 0x06,                   // imul eax, eax, 6
    0x83, 0xE8, 0x02,                   // sub  eax, 2
    0xF4,                               // hlt
];

fn run(make_backend: impl FnOnce() -> Box<dyn Backend>) -> u32 {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: RAM },
            consistency: MemConsistency::Fast,
        },
        make_backend(),
    );
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, CODE).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, ENTRY);
    match cpu.run(&vm, None) {
        Exit::Hlt => cpu.reg(Reg::Rax) as u32,
        other => panic!("unexpected exit: {other:?}"),
    }
}

fn main() {
    let interp = run(|| Box::new(x86jit_core::InterpreterBackend));
    let jit = run(|| Box::new(JitBackend::new()));
    println!("interp eax = {interp}");
    println!("jit    eax = {jit}");
    assert_eq!(interp, jit, "backends diverged");
    assert_eq!(jit, 40);
    println!("backends agree ✓");
}
