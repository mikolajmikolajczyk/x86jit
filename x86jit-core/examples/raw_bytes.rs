//! Smallest possible embedding: hand-assembled x86-64 bytes, run on the
//! interpreter, read back a register. No ELF, no OS — just the core contract
//! (§1): give it a memory map + an entry point, get an `Exit` back.
//!
//! Run with: `cargo run -p x86jit-core --example raw_bytes`

use x86jit_core::{Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig};

const RAM: u64 = 0x1_0000; // 64 KiB flat guest space
const ENTRY: u64 = 0x1000;

fn main() {
    // A flat guest address space with the default interpreter backend.
    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: RAM },
        consistency: MemConsistency::Fast,
    });
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();

    // mov eax, 2   ; B8 02000000
    // add eax, 3   ; 05 03000000
    // hlt          ; F4
    #[rustfmt::skip]
    let code: &[u8] = &[
        0xB8, 0x02, 0x00, 0x00, 0x00,
        0x05, 0x03, 0x00, 0x00, 0x00,
        0xF4,
    ];
    vm.write_bytes(ENTRY, code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, ENTRY);

    // Run until the guest hands control back. `None` = no block budget.
    match cpu.run(&vm, None) {
        Exit::Hlt => println!("guest halted; eax = {}", cpu.reg(Reg::Rax) as u32),
        other => panic!("unexpected exit: {other:?}"),
    }
    assert_eq!(cpu.reg(Reg::Rax) as u32, 5);
}
