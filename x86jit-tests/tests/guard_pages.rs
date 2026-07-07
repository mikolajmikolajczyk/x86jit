//! Guard pages (doc-30, task-127): a JIT (or interp) access to an in-span-but-unmapped
//! guest address on a **host-backed guarded** span hardware-faults (PROT_NONE) and is
//! recovered into a resumable `Exit::UnmappedMemory` by `sigsegv::guarded_run` — closing
//! the decision-3 gap where the JIT silently read demand-zero.

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionCaps,
    RegionKind, Vcpu, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_linux::hostmem::reserve_guarded;
use x86jit_linux::sigsegv::guarded_run;

const CODE: u64 = 0x1000;
const UNMAPPED: u64 = 0x5000; // in-span, never mapped → a PROT_NONE guard page
const SPAN: u64 = 0x10000;

/// Build a guarded Reserved VM, map `CODE` RX, run to the first fault/hlt, and
/// return the exit and the final `Vcpu` (so a test can read any register). The
/// `Vcpu` outlives the dropped `Vm` — it holds registers only, no memory ref.
fn run_regs(backend: Box<dyn Backend>, code: &[u8], budget: u64) -> (Exit, Vcpu) {
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
    let exit = guarded_run(&mut cpu, &vm, Some(budget));
    (exit, cpu)
}

fn run(backend: Box<dyn Backend>, code: &[u8]) -> (Exit, u64) {
    let (exit, cpu) = run_regs(backend, code, 100);
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

/// Build a guarded VM with extra mapped regions, run to the first fault, and
/// return the exit, final `Vcpu`, and how many regions the cache formed. Lets a
/// test read GPRs/RIP and confirm a superblock region actually materialized.
fn run_mapped(
    backend: Box<dyn Backend>,
    code: &[u8],
    extra: &[(u64, usize, Prot)],
    budget: u64,
) -> (Exit, Vcpu, u64) {
    let ram = reserve_guarded(SPAN);
    let mut vm = Vm::with_backend_host_ram(
        VmConfig {
            memory_model: MemoryModel::Reserved { span: SPAN },
            consistency: MemConsistency::Fast,
        },
        backend,
        ram,
    );
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    for &(addr, size, prot) in extra {
        vm.map(addr, size, prot, RegionKind::Ram).unwrap();
    }
    vm.write_bytes(CODE, code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    let exit = guarded_run(&mut cpu, &vm, Some(budget));
    let regions = vm.cache.regions();
    (exit, cpu, regions)
}

/// GP-3: the recovered guest RIP is exact and identical under interp and JIT —
/// on the faulting instruction, not the block entry. Pre-GP-3 the JIT left RIP
/// stale (block entry `CODE`) because a single block only stores RIP on exit.
#[test]
fn guarded_fault_reports_precise_rip_parity() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(ecx, UNMAPPED as i32).unwrap(); // 5 bytes @ CODE
    asm.mov(eax, dword_ptr(rcx)).unwrap(); // faulting load @ CODE+5
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();
    let load_rip = CODE + 5;

    let mut rips = Vec::new();
    for backend in [
        Box::new(InterpreterBackend) as Box<dyn Backend>,
        Box::new(JitBackend::new()),
    ] {
        let (exit, cpu) = run_regs(backend, &code, 100);
        match exit {
            Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, UNMAPPED),
            other => panic!("expected UnmappedMemory, got {other:?}"),
        }
        rips.push(cpu.reg(Reg::Rip));
    }
    assert_eq!(rips[0], load_rip, "interp RIP is on the faulting load");
    assert_eq!(
        rips[1], load_rip,
        "JIT RIP is on the faulting load (GP-3 srcloc)"
    );
}

/// GP-3 through the **region** compiler: a fault mid-superblock resolves to the
/// exact faulting guest RIP (not the region entry), matching the interpreter.
#[test]
fn guarded_region_fault_reports_precise_rip() {
    const DATA: u64 = 0x4000; // mapped RW; the loop walks up into the 0x5000 guard
    let caps = RegionCaps {
        max_blocks: 16,
        max_icount: 256,
    };

    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    asm.mov(rcx, (DATA - 0x1000) as i64).unwrap(); // first `add` → DATA (mapped)
    asm.set_label(&mut top).unwrap();
    asm.add(rcx, 0x1000).unwrap();
    asm.mov(eax, dword_ptr(rcx)).unwrap(); // faults once rcx reaches a guard page
    asm.cmp(rcx, 0x9000i32).unwrap();
    asm.jb(top).unwrap(); // back-edge → loop → a region forms
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();
    let map = [(DATA, 0x1000usize, Prot::RW)];

    let (iexit, icpu, _) = run_mapped(Box::new(InterpreterBackend), &code, &map, 100);
    match iexit {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, UNMAPPED),
        other => panic!("interp: expected UnmappedMemory, got {other:?}"),
    }

    let (jexit, jcpu, regions) = run_mapped(
        Box::new(JitBackend::with_superblocks(caps)),
        &code,
        &map,
        100,
    );
    match jexit {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, UNMAPPED),
        other => panic!("region JIT: expected UnmappedMemory, got {other:?}"),
    }
    assert!(regions > 0, "the loop must form a superblock region");
    assert_eq!(
        jcpu.reg(Reg::Rip),
        icpu.reg(Reg::Rip),
        "region JIT fault RIP matches the interpreter"
    );
    assert!(
        jcpu.reg(Reg::Rip) > CODE,
        "faulting load is inside the loop body, not the region entry"
    );
}

/// GP-3 R2: at a single-block fault the JIT's GPRs match the interpreter —
/// stores *before* the faulting load are committed (write-through), the load's
/// own destination is not (fault-before-commit, as x86 orders it).
#[test]
fn guarded_single_block_fault_preserves_gpr_ordering() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(ebx, 0x1234i32).unwrap(); // committed before the fault
    asm.mov(ecx, UNMAPPED as i32).unwrap();
    asm.mov(eax, dword_ptr(rcx)).unwrap(); // faults; eax stays its old value
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let (_, icpu) = run_regs(Box::new(InterpreterBackend), &code, 100);
    let (_, jcpu) = run_regs(Box::new(JitBackend::new()), &code, 100);
    for reg in [Reg::Rax, Reg::Rbx, Reg::Rcx, Reg::Rip] {
        assert_eq!(jcpu.reg(reg), icpu.reg(reg), "{reg:?} parity at fault");
    }
    assert_eq!(jcpu.reg(Reg::Rbx), 0x1234, "pre-fault store is committed");
    assert_eq!(
        jcpu.reg(Reg::Rax),
        0,
        "faulting load did not commit its dest"
    );
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
