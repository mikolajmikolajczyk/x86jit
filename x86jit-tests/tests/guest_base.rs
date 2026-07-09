//! guest_base identity mapping (task-192): a `Reserved` VM whose RAM buffer is mmap'd
//! at a fixed host address equal to `guest_base` gives **host == guest** identity —
//! `ptr as u64 - guest_base == 0`, so a guest address equals its own host address and
//! the embedder can dereference a raw guest pointer directly. Exercised on both the
//! interpreter and the Cranelift JIT, plus the embedder-side identity proof.

use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;
use x86jit_linux::hostmem::reserve_at;

const GUEST_BASE: u64 = 0x10000;
const SPAN: u64 = 0x0080_0000; // exclusive top guest address (8 MiB)
const CODE: u64 = 0x0040_0000; // where the guest program is mapped/loaded
                               // mov eax, 42 ; hlt
const PROGRAM: &[u8] = &[0xB8, 0x2A, 0x00, 0x00, 0x00, 0xF4];

/// Build a `Reserved` VM with `guest_base = 0x10000` over a real identity mmap, load
/// `mov eax,42; hlt` at `CODE`, run to exit, and return the exit + final RAX (low 32).
fn run(backend: Box<dyn Backend>) -> (Exit, u64) {
    let ram = reserve_at(GUEST_BASE, SPAN);
    assert_eq!(ram.guest_base, GUEST_BASE);
    assert_eq!(
        ram.ptr as u64, GUEST_BASE,
        "identity mmap lands at guest_base"
    );
    let mut vm = Vm::with_backend_host_ram(VmConfig::reserved(SPAN), backend, ram);
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, PROGRAM).unwrap();

    // Embedder-side identity (AC #2): the guest address is a valid host address holding
    // the very bytes we wrote through the VM. `0xB8` is the opcode of `mov eax, imm32`.
    // SAFETY: `CODE` was mapped RX and written above; under identity mapping it is a
    // live host address (`reserve_at` mmap'd `[guest_base, span)` at that host address).
    let first_byte = unsafe { *(CODE as *const u8) };
    assert_eq!(
        first_byte, 0xB8,
        "identity: *(0x400000) is the guest's opcode"
    );

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    let exit = cpu.run(&vm, Some(100));
    (exit, cpu.reg(Reg::Rax) & 0xffff_ffff)
}

/// AC #1/#2: interpreter — Hlt with RAX==42, embedder-side identity proven inside `run`.
#[test]
fn identity_mapping_interpreter() {
    let (exit, rax) = run(Box::new(InterpreterBackend));
    assert!(matches!(exit, Exit::Hlt), "expected Hlt, got {exit:?}");
    assert_eq!(rax, 42);
}

/// AC #1/#2: Cranelift JIT — same result, proving the baked base-relative codegen.
#[test]
fn identity_mapping_jit() {
    let (exit, rax) = run(Box::new(JitBackend::new()));
    assert!(matches!(exit, Exit::Hlt), "expected Hlt, got {exit:?}");
    assert_eq!(rax, 42);
}

/// A guest address below `guest_base` has no backing and must trap rather than
/// dereference a null-adjacent host pointer. `map` below the base is rejected, and a
/// runtime access below the base traps `UnmappedMemory` on both backends.
fn below_base_traps(backend: Box<dyn Backend>) -> Exit {
    let ram = reserve_at(GUEST_BASE, SPAN);
    let mut vm = Vm::with_backend_host_ram(VmConfig::reserved(SPAN), backend, ram);
    // Mapping below the base is refused outright.
    assert!(
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).is_err(),
        "map below guest_base must be rejected"
    );
    // A program that reads a below-base address traps instead of reading garbage.
    // mov ecx, 0x8000 ; mov eax, [rcx] ; hlt   (0x8000 < guest_base 0x10000)
    let prog = [
        0xB9, 0x00, 0x80, 0x00, 0x00, // mov ecx, 0x8000
        0x8B, 0x01, // mov eax, [rcx]
        0xF4, // hlt
    ];
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &prog).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.run(&vm, Some(100))
}

#[test]
fn below_base_access_traps_interpreter() {
    match below_base_traps(Box::new(InterpreterBackend)) {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, 0x8000),
        other => panic!("expected UnmappedMemory at 0x8000, got {other:?}"),
    }
}

#[test]
fn below_base_access_traps_jit() {
    match below_base_traps(Box::new(JitBackend::new())) {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, 0x8000),
        other => panic!("expected UnmappedMemory at 0x8000, got {other:?}"),
    }
}
