//! Modelling a memory-mapped device. A `RegionKind::Trap` region turns every
//! guest load/store into an `Exit` the embedder services, then resumes — the
//! mechanism an emulator uses to back registers, DMA, or interrupt controllers
//! without the core knowing what a device is (§4.2, §5.2).
//!
//! Run with: `cargo run -p x86jit-core --example mmio_device`

use x86jit_core::{Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig};

const RAM: u64 = 0x1_0000;
const ENTRY: u64 = 0x1000;
const DEVICE: u64 = 0x3000; // start of the trapped (MMIO) window

fn main() {
    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: RAM },
        consistency: MemConsistency::Fast,
    });
    // Ordinary RAM everywhere except a 4 KiB trapped device window.
    vm.map(0, DEVICE as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();
    vm.map(DEVICE, 0x1000, Prot::RW, RegionKind::Trap).unwrap();

    // mov eax, [0x3000]   ; 8B 04 25 00300000   -> MmioRead  (device status)
    // mov [0x3004], eax   ; 89 04 25 04300000   -> MmioWrite (device command)
    // hlt                 ; F4
    #[rustfmt::skip]
    let code: &[u8] = &[
        0x8B, 0x04, 0x25, 0x00, 0x30, 0x00, 0x00,
        0x89, 0x04, 0x25, 0x04, 0x30, 0x00, 0x00,
        0xF4,
    ];
    vm.write_bytes(ENTRY, code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, ENTRY);

    // Service device traps until the guest halts. Each MMIO exit leaves RIP on
    // the faulting instruction; the next `run()` retries it.
    loop {
        match cpu.run(&vm, None) {
            Exit::MmioRead { addr, size } => {
                // Device read: synthesize whatever the register should return.
                let value = 0x0000_002A; // e.g. a "ready" status word
                println!("device read  @ {addr:#x} ({size} bytes) -> {value:#x}");
                cpu.complete_mmio_read(value);
            }
            Exit::MmioWrite { addr, size, value } => {
                // Device write: perform the side effect here, then acknowledge it.
                // RIP was left on the store; `complete_mmio_write` lets the retried
                // store skip re-trapping so execution moves on.
                println!("device write @ {addr:#x} ({size} bytes) <- {value:#x}");
                cpu.complete_mmio_write();
            }
            Exit::Hlt => {
                println!("guest halted; eax = {:#x}", cpu.reg(Reg::Rax) as u32);
                break;
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }
}
