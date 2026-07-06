//! task-132 follow-up: an unsupported instruction must **scream, not hide**. The Go
//! netpoller "deadlock" cost hours because a threaded worker's join swallowed a
//! main-thread `rcr` trap. Now the threaded driver (a) logs the unknown opcode's bytes
//! to stderr the instant it traps (`gap:instruction`), and (b) surfaces it as an error
//! within a bounded time instead of hanging. This pins (b) — and exercises (a).

use x86jit_core::{
    InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_linux::LinuxShim;

#[test]
fn unknown_instruction_surfaces_promptly_not_hangs() {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: 0x2000 },
            consistency: MemConsistency::Fast,
        },
        Box::new(InterpreterBackend),
    );
    vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(0x1000, &[0xF9]).unwrap(); // `stc` — currently unlifted
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, 0x1000);
    cpu.set_reg(Reg::Rsp, 0x1f00);

    // Prints `x86jit: UNKNOWN INSTRUCTION at 0x1000: f9 (gap:instruction)` to stderr and
    // returns Err — never hangs (the pre-fix failure mode when a sibling was parked).
    let outcome = x86jit_linux::run_threaded(vm, cpu, LinuxShim::new());
    assert!(
        outcome.is_err(),
        "an unsupported instruction must surface as an error, got {outcome:?}"
    );
}
