//! Self-modifying code (M6, spec §10, testing.md §6): a write onto a page that
//! backs a translated block must invalidate the cache so the next execution
//! re-lifts the changed bytes. Two write sources are covered:
//!
//! - the guest patching its own `.text`, and
//! - an embedder overwriting code between runs (`write_bytes` — loader / syscall
//!   passthrough).
//!
//! **Every guest-self-patch case runs on BOTH backends, from one body** (task-329).
//! It did not use to: JIT-compiled stores wrote host RAM directly and reached no SMC
//! hook at all, so a guest that patched another block and called it ran the stale
//! translation. The suite was green over it because the JIT-backed tests here all
//! write from the *embedder* side, which routes through `Memory::write_bytes` and
//! therefore exercises a path the guest never takes. A one-backend assertion on a
//! two-backend property is how that survived; hence `both_backends!`.
//!
//! What stays deferred is only §10's same-block case: a block that writes into the
//! page it is itself executing runs to the end of that block on the old bytes.

use iced_x86::code_asm::*;
use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x1_0000;
const MAIN: u64 = 0x1000;
const TARGET: u64 = 0x2000;
const STACK_TOP: u64 = 0x8000;

fn assemble(origin: u64, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    build(&mut a);
    a.assemble(origin).unwrap()
}

fn new_vm(backend: Box<dyn Backend>) -> Vm {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
    vm
}

/// Declare one test body and run it on both backends, so a property that holds on the
/// interpreter can never be recorded as the engine's property while the JIT breaks it.
macro_rules! both_backends {
    ($body:ident, $interp:ident, $jit:ident) => {
        #[test]
        fn $interp() {
            $body(Box::new(InterpreterBackend));
        }
        #[test]
        fn $jit() {
            $body(Box::new(JitBackend::new()));
        }
    };
}

fn run_to_hlt(vm: &Vm, cpu: &mut x86jit_core::Vcpu) {
    match cpu.run(vm, None) {
        Exit::Hlt => {}
        other => panic!("unexpected exit: {other:?}"),
    }
}

/// The guest overwrites its own code: it calls `target` (caching that block),
/// patches `target`'s first instruction from `mov eax, 1` to `mov eax, 2`, then
/// calls `target` again. Without SMC invalidation the second call would replay
/// the stale cached `eax = 1`; with it, the engine re-lifts and yields `eax = 2`.
fn guest_self_modification(backend: Box<dyn Backend>) {
    let vm = new_vm(backend);

    // target: `mov eax, 1; ret`  ->  B8 01 00 00 00 C3
    let target = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.ret().unwrap();
    });
    vm.write_bytes(TARGET, &target).unwrap();

    let main = assemble(MAIN, |a| {
        a.mov(r15, TARGET).unwrap();
        a.call(r15).unwrap(); // run target v1 (eax = 1), caches its block
                              // patch target's first 5 bytes to `mov eax, 2` (B8 02 00 00 00)
        a.mov(byte_ptr(TARGET), 0xB8i32).unwrap();
        a.mov(dword_ptr(TARGET + 1), 2i32).unwrap();
        a.call(r15).unwrap(); // run target v2 — must observe eax = 2
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    cpu.set_reg(Reg::Rsp, STACK_TOP);
    run_to_hlt(&vm, &mut cpu);

    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        2,
        "second call must run the patched code"
    );
    assert!(
        vm.cache.misses() >= 2,
        "target must have been lifted twice (initial + re-lift)"
    );
}

both_backends!(
    guest_self_modification,
    guest_self_modification_interp,
    guest_self_modification_jit
);

/// An embedder overwrites a cached block between runs via `write_bytes` (the
/// loader / syscall-passthrough path). This works on both backends — the write
/// routes through the SMC hook regardless of who executes the code.
fn embedder_rewrite_reexecutes(backend: Box<dyn Backend>) {
    let vm = new_vm(backend);

    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");

    // Overwrite the block with `mov eax, 42; hlt` and re-run from the same entry.
    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "re-run must see the rewritten code"
    );
}

#[test]
fn embedder_rewrite_reexecutes_interp() {
    embedder_rewrite_reexecutes(Box::new(InterpreterBackend));
}

#[test]
fn embedder_rewrite_reexecutes_jit() {
    embedder_rewrite_reexecutes(Box::new(JitBackend::new()));
}

/// A *chained* edge must survive SMC: block MAIN ends in a direct `jmp TARGET`,
/// so after the first run its link slot points at TARGET's compiled entry. When
/// the embedder rewrites TARGET (a different code page — MAIN's block is NOT
/// invalidated), the next run of MAIN must NOT chain into TARGET's stale compiled
/// code. Requires `handle_smc` to clear the backend's link slots on invalidation
/// (otherwise the filled slot returns `RET_CHAIN` into the dropped block). JIT
/// only — the interpreter has no link slots.
#[test]
fn stale_link_slot_cleared_on_invalidation() {
    let vm = new_vm(Box::new(JitBackend::new()));

    // MAIN (page 0x1000): jump straight to TARGET (a direct, chainable edge).
    let main = assemble(MAIN, |a| {
        a.jmp(TARGET).unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    // TARGET (page 0x2000): mov eax, 1; hlt.
    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    // First run: MAIN links to TARGET (slot filled), TARGET yields eax = 1.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");

    // Embedder rewrites ONLY TARGET (mov eax, 42; hlt). MAIN's page is untouched,
    // so MAIN's compiled block — and its filled link slot — survive.
    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();
    let misses_before = vm.cache.misses();

    // Second run from MAIN: the stale slot must not be followed. With the fix,
    // SMC clears the slot, MAIN re-links, TARGET is re-lifted → eax = 42.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "chained edge must re-resolve the rewritten TARGET, not run stale code"
    );
    assert!(
        vm.cache.misses() > misses_before,
        "TARGET must have been re-lifted after invalidation"
    );
}

/// The per-vcpu fast-resolve cache (fast-dispatch R3) must not outlive an invalidation:
/// the SAME vcpu runs a block, the embedder rewrites it, and the vcpu runs it
/// again. Without the invalidation-epoch flush the vcpu's fast cache would serve
/// the stale compiled entry; with it, the cache flushes and the block re-lifts.
#[test]
fn fast_resolve_cache_flushes_on_invalidation() {
    let vm = new_vm(Box::new(JitBackend::new()));

    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    // One vcpu, reused across both runs, so its fast-resolve cache persists.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");

    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();
    let misses_before = vm.cache.misses();

    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "same vcpu must re-lift the rewritten block, not serve its stale fast entry"
    );
    assert!(
        vm.cache.misses() > misses_before,
        "the rewritten block must have been re-lifted"
    );
}

/// IBTC slots (fast-dispatch R4) inherit the same SMC coherence as link slots: an
/// indirect `jmp reg` fills a per-site descriptor pointing at TARGET's compiled
/// entry; when the embedder rewrites TARGET, the next run must not chain through
/// the stale descriptor. `invalidate_links` zeroes the IBTC slot (in the same
/// arena as link slots), so the site re-resolves. JIT only.
#[test]
fn stale_ibtc_descriptor_cleared_on_invalidation() {
    let vm = new_vm(Box::new(JitBackend::new()));

    // MAIN (page 0x1000): mov rdx, TARGET; jmp rdx  — a monomorphic indirect jump.
    let main = assemble(MAIN, |a| {
        a.mov(rdx, TARGET).unwrap();
        a.jmp(rdx).unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    let v1 = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v1).unwrap();

    // First run: the jmp reg fills its IBTC slot with {TARGET, v1 entry}, eax = 1.
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(cpu.reg(Reg::Rax) as u32, 1, "first run");
    assert!(vm.cache.ibtc_filled() >= 1, "IBTC must have fired");

    // Rewrite ONLY TARGET; MAIN (and its filled IBTC slot) survive.
    let v2 = assemble(TARGET, |a| {
        a.mov(eax, 42i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &v2).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        42,
        "indirect edge must re-resolve the rewritten TARGET, not run stale code"
    );
}

/// A guest `rep stosb` that overwrites its own cached code must invalidate it,
/// exactly like a scalar store (#4). Before the fix the interpreter's string ops
/// wrote guest RAM through a raw pointer that bypassed `Memory::write`'s SMC
/// `note_write`, so a self-modifying `rep stos` left the stale block cached and
/// replayed it. `target` is `mov al, 1; ret`; the guest patches its immediate byte
/// to 42 with a one-element `rep stosb`, then re-calls it.
fn self_modification_via_rep_stos(backend: Box<dyn Backend>) {
    let vm = new_vm(backend);

    // target: `mov al, 1; ret`  ->  B0 01 C3  (imm at TARGET+1)
    let target = assemble(TARGET, |a| {
        a.mov(al, 1i32).unwrap();
        a.ret().unwrap();
    });
    vm.write_bytes(TARGET, &target).unwrap();

    let main = assemble(MAIN, |a| {
        a.mov(r15, TARGET).unwrap();
        a.call(r15).unwrap(); // run target v1 (al = 1), caches + marks its page
                              // patch target's immediate byte to 42 via `rep stosb` (AL=42, one element)
        a.mov(al, 42i32).unwrap();
        a.mov(edi, (TARGET + 1) as u32).unwrap();
        a.mov(ecx, 1u32).unwrap();
        a.cld().unwrap();
        a.rep().stosb().unwrap();
        a.call(r15).unwrap(); // run target v2 — must observe al = 42
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    cpu.set_reg(Reg::Rsp, STACK_TOP);
    run_to_hlt(&vm, &mut cpu);

    assert_eq!(
        cpu.reg(Reg::Rax) as u8,
        42,
        "second call must run the rep-stos-patched code"
    );
    assert!(
        vm.cache.misses() >= 2,
        "target must have been lifted twice (initial + re-lift after rep stos)"
    );
}

both_backends!(
    self_modification_via_rep_stos,
    self_modification_via_rep_stos_interp,
    self_modification_via_rep_stos_jit
);

/// A guest x87 store that overwrites its own cached code must invalidate it, like
/// a scalar store (#4). Before the fix the x87 helper wrote guest RAM through a raw
/// pointer that skipped `Memory`'s SMC `note_write`. `target` is `mov eax, 1; ret`;
/// the guest rewrites its 32-bit immediate to 2 with `fild`/`fistp dword`, then
/// re-calls it.
fn self_modification_via_x87_store(backend: Box<dyn Backend>) {
    const SCRATCH: u64 = 0x3000; // holds the integer to store (a non-code page)
    let vm = new_vm(backend);

    // target: `mov eax, 1; ret`  ->  B8 01 00 00 00 C3  (imm32 at TARGET+1)
    let target = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.ret().unwrap();
    });
    vm.write_bytes(TARGET, &target).unwrap();
    vm.write_bytes(SCRATCH, &2u32.to_le_bytes()).unwrap();

    let main = assemble(MAIN, |a| {
        a.mov(r15, TARGET).unwrap();
        a.call(r15).unwrap(); // run target v1 (eax = 1), caches + marks its page
                              // patch target's imm32 to 2 via an x87 int store
        a.fild(dword_ptr(SCRATCH)).unwrap(); // ST0 = 2.0
        a.fistp(dword_ptr(TARGET + 1)).unwrap(); // write i32 2 into the immediate
        a.call(r15).unwrap(); // run target v2 — must observe eax = 2
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &main).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    cpu.set_reg(Reg::Rsp, STACK_TOP);
    run_to_hlt(&vm, &mut cpu);

    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        2,
        "second call must run the x87-patched code"
    );
    assert!(
        vm.cache.misses() >= 2,
        "target must have been lifted twice (initial + re-lift after x87 store)"
    );
}

both_backends!(
    self_modification_via_x87_store,
    self_modification_via_x87_store_interp,
    self_modification_via_x87_store_jit
);

/// An MMIO read yields `Exit::MmioRead`, and after `complete_mmio_read` the guest
/// resumes and the retried load gets the supplied value (§5.2) — the embedder
/// resume path, previously a `todo!()` panic.
#[test]
fn mmio_read_resumes_with_the_supplied_value() {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(InterpreterBackend));
    vm.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
    vm.map(0x3000, 0x1000, Prot::RW, RegionKind::Trap).unwrap();

    // mov eax, [0x3000]  (an MMIO read);  hlt
    let code = assemble(MAIN, |a| {
        a.mov(eax, dword_ptr(0x3000u64)).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    match cpu.run(&vm, None) {
        Exit::MmioRead { addr, size } => {
            assert_eq!(addr, 0x3000);
            assert_eq!(size, 4);
        }
        other => panic!("expected MmioRead, got {other:?}"),
    }
    // Deliver the value; the guest re-executes the load and gets it, then hlts.
    cpu.complete_mmio_read(0xDEAD_BEEF);
    match cpu.run(&vm, None) {
        Exit::Hlt => {}
        other => panic!("expected Hlt after resume, got {other:?}"),
    }
    assert_eq!(
        cpu.reg(Reg::Rax),
        0xDEAD_BEEF,
        "load consumed the MMIO value"
    );
}

/// A `rep stos` into a `Trap` (MMIO) region must yield `Exit::MmioWrite`, not
/// silently scribble the backing buffer (#4). The raw string path bypassed the
/// region check `Memory::write` performs; routing through it traps on the first
/// element with RIP left on the `rep` instruction (restartable).
#[test]
fn rep_stos_into_mmio_region_traps() {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(InterpreterBackend));
    // Code/data RAM in [0x1000, 0x3000); an MMIO trap region in [0x3000, 0x4000).
    vm.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
    vm.map(0x3000, 0x1000, Prot::RW, RegionKind::Trap).unwrap();

    let code = assemble(MAIN, |a| {
        a.mov(al, 0xABi32).unwrap();
        a.mov(edi, 0x3000u32).unwrap();
        a.mov(ecx, 4u32).unwrap();
        a.cld().unwrap();
        a.rep().stosb().unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    match cpu.run(&vm, None) {
        Exit::MmioWrite { addr, size, value } => {
            assert_eq!(addr, 0x3000, "trap at the first MMIO byte");
            assert_eq!(size, 1, "stosb element width");
            assert_eq!(value, 0xAB, "the AL byte being stored");
        }
        other => panic!("expected MmioWrite, got {other:?}"),
    }
    assert_eq!(
        cpu.reg(Reg::Rcx),
        4,
        "no element committed before the trap (restartable)"
    );
}

/// After an `Exit::MmioWrite`, `complete_mmio_write` lets the guest resume: the
/// retried store skips re-trapping (side effect already done by the embedder) and
/// execution continues. A following RAM write proves progress past the store.
#[test]
fn mmio_write_resumes_and_continues() {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(InterpreterBackend));
    vm.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
    vm.map(0x3000, 0x1000, Prot::RW, RegionKind::Trap).unwrap();

    // mov eax, 0x12345678 ; mov [0x3004], eax (MMIO) ; mov [0x1500], eax (RAM) ; hlt
    let code = assemble(MAIN, |a| {
        a.mov(eax, 0x1234_5678u32).unwrap();
        a.mov(dword_ptr(0x3004u64), eax).unwrap();
        a.mov(dword_ptr(0x1500u64), eax).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    match cpu.run(&vm, None) {
        Exit::MmioWrite { addr, size, value } => {
            assert_eq!(addr, 0x3004);
            assert_eq!(size, 4);
            assert_eq!(value, 0x1234_5678);
        }
        other => panic!("expected MmioWrite, got {other:?}"),
    }
    // Acknowledge the side effect; the store must not re-trap on resume.
    cpu.complete_mmio_write();
    match cpu.run(&vm, None) {
        Exit::Hlt => {}
        other => panic!("expected Hlt after resume, got {other:?}"),
    }
    let mut buf = [0u8; 4];
    vm.read_bytes(0x1500, &mut buf).unwrap();
    assert_eq!(
        u32::from_le_bytes(buf),
        0x1234_5678,
        "the RAM store after the MMIO write executed — resume made progress"
    );
}

/// JIT-side MMIO (§5.2, M4-T10): an inlined load into a `Trap` region is deferred
/// to the interpreter, which yields `MmioRead`; on resume the deferred load returns
/// the supplied value — identical to the interpreter backend.
#[test]
fn mmio_read_resumes_on_jit() {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(JitBackend::new()));
    vm.map(0x1000, 0x2000, Prot::RWX, RegionKind::Ram).unwrap();
    vm.map(0x3000, 0x1000, Prot::RW, RegionKind::Trap).unwrap();

    let code = assemble(MAIN, |a| {
        a.mov(eax, dword_ptr(0x3000u64)).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    match cpu.run(&vm, None) {
        Exit::MmioRead { addr, size } => {
            assert_eq!(addr, 0x3000);
            assert_eq!(size, 4);
        }
        other => panic!("expected MmioRead under JIT, got {other:?}"),
    }
    cpu.complete_mmio_read(0xDEAD_BEEF);
    match cpu.run(&vm, None) {
        Exit::Hlt => {}
        other => panic!("expected Hlt after JIT resume, got {other:?}"),
    }
    assert_eq!(
        cpu.reg(Reg::Rax),
        0xDEAD_BEEF,
        "JIT load consumed the MMIO value"
    );
}

/// JIT-side MMIO write (M4-T10): the inlined store defers, yields `MmioWrite`, and
/// after `complete_mmio_write` the guest resumes and a following RAM store lands.
#[test]
fn mmio_write_resumes_on_jit() {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(JitBackend::new()));
    vm.map(0x1000, 0x2000, Prot::RWX, RegionKind::Ram).unwrap();
    vm.map(0x3000, 0x1000, Prot::RW, RegionKind::Trap).unwrap();

    let code = assemble(MAIN, |a| {
        a.mov(eax, 0x1234_5678u32).unwrap();
        a.mov(dword_ptr(0x3004u64), eax).unwrap();
        a.mov(dword_ptr(0x1500u64), eax).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN);
    match cpu.run(&vm, None) {
        Exit::MmioWrite { addr, size, value } => {
            assert_eq!(addr, 0x3004);
            assert_eq!(size, 4);
            assert_eq!(value, 0x1234_5678);
        }
        other => panic!("expected MmioWrite under JIT, got {other:?}"),
    }
    cpu.complete_mmio_write();
    match cpu.run(&vm, None) {
        Exit::Hlt => {}
        other => panic!("expected Hlt after JIT resume, got {other:?}"),
    }
    let mut buf = [0u8; 4];
    vm.read_bytes(0x1500, &mut buf).unwrap();
    assert_eq!(
        u32::from_le_bytes(buf),
        0x1234_5678,
        "JIT resume made progress"
    );
}

/// `Vm::unmap` must invalidate blocks cached from the unmapped range (#15A), so a
/// later execution faults instead of running the stale translation. The block is
/// cached by a first run, the region is unmapped, and a re-run must not return `Hlt`.
#[test]
fn unmap_invalidates_cached_blocks() {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(InterpreterBackend));
    vm.map(TARGET, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    let code = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu); // caches TARGET's block

    vm.unmap(TARGET, 0x1000).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    let exit = cpu.run(&vm, None);
    assert!(
        !matches!(exit, Exit::Hlt),
        "must not run the stale cached block after unmap: {exit:?}"
    );
}

/// A write to a NON-code page must not perturb the cache (no false invalidation).
#[test]
fn write_to_data_page_does_not_invalidate() {
    let vm = new_vm(Box::new(InterpreterBackend));
    let code = assemble(TARGET, |a| {
        a.mov(eax, 7i32).unwrap();
        a.mov(dword_ptr(0x4000u64), eax).unwrap(); // store to a far data page
        a.hlt().unwrap();
    });
    vm.write_bytes(TARGET, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    let misses_after_first = vm.cache.misses();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, TARGET);
    run_to_hlt(&vm, &mut cpu);
    assert_eq!(
        vm.cache.misses(),
        misses_after_first,
        "data-page write must not re-lift code"
    );
    assert!(vm.cache.hits() >= 1, "second run should hit the cache");
}

/// The two page-boundary cases the JIT's inline SMC gate can get wrong (task-329).
///
/// That gate is a watermark — `(page - lo) < len` on the page of the store's FIRST
/// byte — so two shapes have to be pinned by execution rather than by reading it:
///
/// - **A store that starts one page BELOW the code range and spills into it.** Its
///   first byte's page is not code, so a naive range holding exactly the code extent
///   rejects it and the patch to the range's lowest page is lost. `widen_code_range`
///   keeps `lo` one page low precisely for this; a store is at most 64 bytes against a
///   4096-byte page, so one page of skew is exactly enough and no more.
/// - **A store to the HIGHEST code page**, the other end of the same compare.
///
/// The layout puts `main` BETWEEN the two targets so the low target really is the
/// bottom of the range and the page below it really is not code.
fn smc_at_the_edges_of_the_code_range(backend: Box<dyn Backend>) {
    const TARGET_LO: u64 = 0x3000; // lowest code page; page 0x2 below it is data
    const MAIN_MID: u64 = 0x5000;
    const TARGET_HI: u64 = 0x6000; // highest code page

    let vm = new_vm(backend);
    for at in [TARGET_LO, TARGET_HI] {
        let code = assemble(at, |a| {
            a.mov(eax, 1i32).unwrap(); // B8 01 00 00 00 — imm32 at at+1
            a.ret().unwrap();
        });
        vm.write_bytes(at, &code).unwrap();
    }

    let main = assemble(MAIN_MID, |a| {
        // Cache and mark both targets, so the code range spans pages 3..=6.
        a.mov(r15, TARGET_LO).unwrap();
        a.call(r15).unwrap();
        a.mov(r14, TARGET_HI).unwrap();
        a.call(r14).unwrap();

        // Straddle: a dword at TARGET_LO-1 writes 0x2fff..0x3002, so its FIRST byte
        // is on the data page below the code range. Little-endian 0x0002_b800 lands
        // B8 02 00 over TARGET_LO..+2, turning `mov eax,1` into `mov eax,2`.
        a.mov(dword_ptr(TARGET_LO - 1), 0x0002_b800u32 as i32)
            .unwrap();
        // Wholly inside the range's top page.
        a.mov(dword_ptr(TARGET_HI + 1), 3i32).unwrap();

        a.call(r15).unwrap();
        a.mov(ebx, eax).unwrap(); // ebx = low target's result
        a.call(r14).unwrap(); // eax = high target's result
        a.hlt().unwrap();
    });
    vm.write_bytes(MAIN_MID, &main).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, MAIN_MID);
    cpu.set_reg(Reg::Rsp, STACK_TOP);
    run_to_hlt(&vm, &mut cpu);

    assert_eq!(
        cpu.reg(Reg::Rbx) as u32,
        2,
        "a store straddling into the LOWEST code page must invalidate it"
    );
    assert_eq!(
        cpu.reg(Reg::Rax) as u32,
        3,
        "a store to the HIGHEST code page must invalidate it"
    );
}

both_backends!(
    smc_at_the_edges_of_the_code_range,
    smc_at_the_edges_of_the_code_range_interp,
    smc_at_the_edges_of_the_code_range_jit
);
