//! Guard pages (doc-30, task-127): a JIT (or interp) access to an in-span-but-unmapped
//! guest address on a **host-backed guarded** span hardware-faults (PROT_NONE) and is
//! recovered into a resumable `Exit::UnmappedMemory` by `sigsegv::guarded_run` — closing
//! the decision-3 gap where the JIT silently read demand-zero.

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_linux::hostmem::reserve_guarded;
use x86jit_linux::sigsegv::guarded_run;

const CODE: u64 = 0x1000;
const UNMAPPED: u64 = 0x5000; // in-span, never mapped → a PROT_NONE guard page
const SPAN: u64 = 0x10000;

fn run(backend: Box<dyn Backend>, code: &[u8]) -> (Exit, u64) {
    let ram = reserve_guarded(SPAN);
    let mut vm = Vm::with_backend_host_ram(
        VmConfig {
            memory_model: MemoryModel::Reserved { span: SPAN },
            consistency: MemConsistency::Fast,
        },
        backend,
        ram,
    );
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap(); // opened → readable
    vm.write_bytes(CODE, code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    let exit = guarded_run(&mut cpu, &vm, Some(100));
    (exit, cpu.reg(Reg::Rax) & 0xffff_ffff)
}

/// Both backends now fault on the guarded in-span load — interp via `region_at`, JIT via
/// the SIGSEGV→`guarded_run` path. (Was: interp `UnmappedMemory`, JIT `Hlt`+EAX=0.)
#[test]
fn guarded_in_span_load_faults_interp_and_jit() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(ecx, UNMAPPED as i32).unwrap();
    asm.mov(eax, dword_ptr(rcx)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    for backend in [
        Box::new(InterpreterBackend) as Box<dyn Backend>,
        Box::new(JitBackend::new()),
    ] {
        match run(backend, &code).0 {
            Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, UNMAPPED),
            other => panic!("expected UnmappedMemory at {UNMAPPED:#x}, got {other:?}"),
        }
    }
}

/// A guarded in-span *store* faults the same way, reported as a Write.
#[test]
fn guarded_in_span_store_faults_on_jit() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(ecx, UNMAPPED as i32).unwrap();
    asm.mov(dword_ptr(rcx), 0x1234i32).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    match run(Box::new(JitBackend::new()), &code).0 {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, UNMAPPED),
        other => panic!("expected UnmappedMemory store, got {other:?}"),
    }
}

/// A nil-deref (guest address 0 — Go's case) faults under the JIT: page 0 is in-span but
/// unmapped (guarded), so the load traps instead of silently reading zero.
#[test]
fn guarded_nil_deref_faults_on_jit() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.xor(ecx, ecx).unwrap();
    asm.mov(eax, dword_ptr(rcx)).unwrap(); // load [0]
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    match run(Box::new(JitBackend::new()), &code).0 {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, 0),
        other => panic!("expected UnmappedMemory at 0 (nil-deref), got {other:?}"),
    }
}

/// Honesty: a SIGSEGV whose address is OUTSIDE every guest span (a genuine host bug)
/// must NOT be swallowed — the handler re-raises so the process dies by signal with its
/// core dump. Verified in a subprocess (the fault is fatal by design).
#[test]
fn host_fault_outside_span_still_crashes() {
    use std::os::unix::process::ExitStatusExt;
    if std::env::var_os("X86JIT_GUARD_CRASH_CHILD").is_some() {
        // Install the handler (a trivial guarded run), then dereference a wild HOST
        // pointer far outside the guest span.
        let ram = reserve_guarded(SPAN);
        let mut vm = Vm::with_backend_host_ram(
            VmConfig {
                memory_model: MemoryModel::Reserved { span: SPAN },
                consistency: MemConsistency::Fast,
            },
            Box::new(InterpreterBackend),
            ram,
        );
        vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &[0xf4]).unwrap(); // hlt
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        let _ = guarded_run(&mut cpu, &vm, Some(1)); // installs the SIGSEGV handler
                                                     // A high canonical address, essentially never mapped and far from the span.
        let wild = 0x5000_0000_0000u64 as *const u8;
        // SAFETY: deliberately faulting — the point is that it crashes honestly.
        unsafe { std::ptr::read_volatile(wild) };
        std::process::exit(0); // only reached if the fault was wrongly swallowed
    }
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(exe)
        .args([
            "host_fault_outside_span_still_crashes",
            "--exact",
            "--nocapture",
        ])
        .env("X86JIT_GUARD_CRASH_CHILD", "1")
        .status()
        .unwrap();
    assert_eq!(
        status.signal(),
        Some(11), // SIGSEGV
        "a host fault outside the guest span must crash (not be swallowed); got {status:?}"
    );
}
