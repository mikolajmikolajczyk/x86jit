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
/// pages in the embedder (decision-7). Out-of-span is where both tiers must agree.
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

/// The x87 tag word after `fninit` is **wrong**, and this pins how wrong so it cannot drift.
///
/// `tag_word` derives tags from the live `fpr[]` bytes, which can express valid / zero /
/// special but never `11` (empty) — this FPU has no architectural stack-emptiness state.
/// Measured on a real CPU: `fninit; fnstenv` gives `0xffff` and `fninit; fld1; fnstenv`
/// gives `0x3fff`; we give `0x5555` and `0x1555`. A guest reading the tag word to count
/// occupied slots therefore sees "all eight hold zero" instead of "all eight are empty".
///
/// This asserts the *current* values deliberately. When stack-emptiness lands, this test
/// must fail and be updated to the hardware column — that is the point of pinning it.
#[test]
fn x87_tag_word_after_fninit_diverges_from_hardware() {
    for (name, code, ours, hardware) in [
        (
            "fninit;fnstenv",
            vec![0xDB, 0xE3, 0xD9, 0x30],
            0x5555u16,
            0xffffu16,
        ),
        (
            "fninit;fld1;fnstenv",
            vec![0xDB, 0xE3, 0xD9, 0xE8, 0xD9, 0x30],
            0x1555,
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
        assert_ne!(
            tw, hardware,
            "{name}: tag word now matches hardware — good, update this test"
        );
        assert_eq!(tw, ours, "{name}: known-divergent tag word changed value");
    }
}
