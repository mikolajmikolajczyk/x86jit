//! `Prot` is advisory — nothing enforces it (task-330).
//!
//! This asserts the CURRENT behaviour on purpose, the way `smc.rs` and `x87_faults.rs`
//! have pinned known divergences before it: a guest store into a region mapped `R` or
//! `RX` succeeds and changes the bytes. If someone later enforces protection, this test
//! fails and forces the records that call it advisory — the `Prot` doc comment,
//! `deferred.md`, the README's known gaps — to be updated with it, rather than being left
//! behind claiming something that stopped being true.
//!
//! Both backends, because "the interpreter could check `region_at` cheaply" is exactly
//! the fix that would create a divergence: the JIT's inlined accesses bound against
//! `MemCtx.size` alone and have no region map by design (decision-3). Whatever happens
//! here must happen on both.

use iced_x86::code_asm::*;
use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const SPAN: u64 = 0x1_0000;
const CODE: u64 = 0x1000;
const PROTECTED: u64 = 0x3000;
const SENTINEL: u32 = 0xDEAD_BEEF;

fn store_into(prot: Prot, backend: Box<dyn Backend>) -> (Exit, u32) {
    let mut vm = Vm::with_backend(VmConfig::flat(SPAN), backend);
    vm.map(0, PROTECTED as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();
    vm.map(PROTECTED, 0x1000, prot, RegionKind::Ram).unwrap();
    vm.map(
        PROTECTED + 0x1000,
        (SPAN - PROTECTED - 0x1000) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();

    let mut a = CodeAssembler::new(64).unwrap();
    a.mov(dword_ptr(PROTECTED), SENTINEL as i32).unwrap();
    a.hlt().unwrap();
    vm.write_bytes(CODE, &a.assemble(CODE).unwrap()).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    let exit = cpu.run(&vm, None);

    let mut buf = [0u8; 4];
    vm.read_bytes(PROTECTED, &mut buf).unwrap();
    (exit, u32::from_le_bytes(buf))
}

fn a_write_to_a_non_writable_region_succeeds(backend: fn() -> Box<dyn Backend>) {
    for prot in [Prot::R, Prot::RX] {
        let (exit, seen) = store_into(prot, backend());
        assert!(
            matches!(exit, Exit::Hlt),
            "{prot:?}: the store did not fault — but if it now DOES, this test is the \
             record that has to change first, together with the Prot doc comment, \
             deferred.md and the README. Got {exit:?}"
        );
        assert_eq!(
            seen, SENTINEL,
            "{prot:?}: the bytes changed, which is what makes `Prot` advisory rather \
             than merely unchecked"
        );
    }
}

#[test]
fn a_write_to_a_non_writable_region_succeeds_interp() {
    a_write_to_a_non_writable_region_succeeds(|| Box::new(InterpreterBackend));
}

#[test]
fn a_write_to_a_non_writable_region_succeeds_jit() {
    a_write_to_a_non_writable_region_succeeds(|| Box::new(JitBackend::new()));
}
