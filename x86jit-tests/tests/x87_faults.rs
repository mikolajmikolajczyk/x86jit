//! x87 memory operands must fault, not silently no-op.
//!
//! `exec_x87` reports a fault by returning `Some((addr, is_write))` and success by
//! returning `None`. Its `read_n` helper uses the *opposite* convention — `None` means the
//! load faulted — so writing `read_n(..)?` propagates a fault as success. Every x87 read
//! did exactly that: an unmapped operand left the FPU untouched and advanced RIP past the
//! instruction, so a guest dereferencing a bad pointer through `fld`, `fild`, `fldcw` or
//! any of the memory-operand arithmetic forms kept running on stale data instead of
//! trapping. The store paths were always correct, which is what made the asymmetry easy to
//! miss.
//!
//! Found by an adversarial review of the x87 integer-arithmetic lift; the defect predates
//! it and covered every x87 memory read.

use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const CODE: u64 = 0x1000;
/// Outside the guest span entirely. In-span-but-unmapped is deliberately NOT used here:
/// the JIT's `RawFpMem` is a bounds-only view with no region map, so it reads demand-zero
/// there rather than faulting — the documented decision-3 divergence, whose fix is guard
/// pages in the embedder (decision-5). Out-of-span is where both tiers must agree.
const BAD: u64 = 0x20_0000;

fn faults_on(bytes: &[u8], backend: Box<dyn Backend>) -> Exit {
    let mut vm = Vm::with_backend(VmConfig::flat(0x10_0000), backend);
    vm.map(CODE, 0x1000, Prot::RWX, RegionKind::Ram).unwrap();
    let mut code = bytes.to_vec();
    code.push(0xF4); // hlt — reached only if the fault was swallowed
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rax, BAD);
    cpu.run(&vm, None)
}

/// Each case is an x87 instruction whose only memory access is a READ.
fn cases() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("fld qword [rax]", vec![0xDD, 0x00]),
        ("fld dword [rax]", vec![0xD9, 0x00]),
        ("fld tbyte [rax]", vec![0xDB, 0x28]),
        ("fild word [rax]", vec![0xDF, 0x00]),
        ("fild dword [rax]", vec![0xDB, 0x00]),
        ("fild qword [rax]", vec![0xDF, 0x28]),
        ("fadd qword [rax]", vec![0xDC, 0x00]),
        ("fadd dword [rax]", vec![0xD8, 0x00]),
        ("fiadd word [rax]", vec![0xDE, 0x00]),
        ("fiadd dword [rax]", vec![0xDA, 0x00]),
        ("fldcw [rax]", vec![0xD9, 0x28]),
    ]
}

#[test]
fn x87_memory_reads_fault_on_the_interpreter() {
    for (name, code) in cases() {
        let e = faults_on(&code, Box::new(InterpreterBackend));
        assert!(
            matches!(e, Exit::UnmappedMemory { .. }),
            "{name}: expected a fault, got {e:?} — a swallowed fault runs the guest on stale state"
        );
    }
}

#[test]
fn x87_memory_reads_fault_on_the_jit() {
    for (name, code) in cases() {
        let e = faults_on(&code, Box::new(JitBackend::new()));
        assert!(
            matches!(e, Exit::UnmappedMemory { .. }),
            "{name}: expected a fault on the JIT, got {e:?}"
        );
    }
}

/// The x87 tag word reports `11`/empty, matching hardware (task-324 AC#3).
///
/// It could not, until `CpuState::fpu_empty` made stack emptiness real state: tags were
/// derived from the live `fpr[]` bytes, which express valid / zero / special but never
/// empty. This test used to assert the divergent values ON PURPOSE — `0x5555` where
/// hardware gives `0xffff`, `0x1555` where it gives `0x3fff` — so that landing the fix
/// would break it. It did, and these are the hardware values it was pinned against.
///
/// Both were measured on a real CPU. `fninit` leaves every register tagged empty (SDM
/// Vol 2A FINIT/FNINIT), so all eight tags are `11`; one `fld1` fills R7, whose tag
/// becomes `00` (valid), leaving `0x3fff`.
#[test]
fn x87_tag_word_after_fninit_matches_hardware() {
    for (name, code, hardware) in [
        ("fninit;fnstenv", vec![0xDB, 0xE3, 0xD9, 0x30], 0xffffu16),
        (
            "fninit;fld1;fnstenv",
            vec![0xDB, 0xE3, 0xD9, 0xE8, 0xD9, 0x30],
            0x3fff,
        ),
    ] {
        let mut vm = Vm::with_backend(VmConfig::flat(0x10_0000), Box::new(InterpreterBackend));
        vm.map(CODE, 0x2000, Prot::RWX, RegionKind::Ram).unwrap();
        let mut c = code.clone();
        c.push(0xF4);
        vm.write_bytes(CODE, &c).unwrap();
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        cpu.set_reg(Reg::Rax, 0x2000);
        assert!(
            matches!(cpu.run(&vm, None), Exit::Hlt),
            "{name} did not run"
        );
        let mut env = [0u8; 28];
        vm.read_bytes(0x2000, &mut env).unwrap();
        let tw = u16::from_le_bytes([env[8], env[9]]);
        assert_eq!(tw, hardware, "{name}: tag word");
    }
}
