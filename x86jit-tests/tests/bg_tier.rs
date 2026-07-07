//! Background tier-up (bg-tier, doc-27 BGT-3): the dispatcher submits a hot block to
//! the backend's compiler thread and swaps it in when it lands, instead of compiling
//! inline. Opt-in (`Vm::set_tier_up_background`), so these tests drive it explicitly;
//! the default-off corpus is unaffected (AC#4). Determinism comes from
//! `JitBackend::tier_up_handle().wait_idle()` — no sleeps.

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionCaps,
    RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const CODE: u64 = 0x1000;

fn vm_with(backend: Box<dyn Backend>, tier: u32, background: bool, prog: &[u8]) -> Vm {
    vm_with_flat(backend, tier, background, prog, 0x2000)
}

fn vm_with_flat(
    backend: Box<dyn Backend>,
    tier: u32,
    background: bool,
    prog: &[u8],
    flat: u64,
) -> Vm {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: flat },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.set_tier_up_after(Some(tier));
    vm.set_tier_up_background(background);
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, prog).unwrap();
    vm
}

/// `mov eax, 42 ; hlt` — one block, re-runnable by resetting RIP.
fn single_block() -> Vec<u8> {
    single_block_val(42)
}

/// `mov eax, <v> ; hlt` at `CODE`, six bytes — so overwriting one payload with
/// another keeps the block length identical (SMC in place).
fn single_block_val(v: i32) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    a.mov(eax, v).unwrap();
    a.hlt().unwrap();
    a.assemble(CODE).unwrap()
}

/// Heat the single block until it submits its background compile: pass 1 caches it
/// (no bump), passes 2-3 bump to 2, pass 4 bumps to 3 (== threshold) and submits.
fn heat_to_submit(vm: &Vm, cpu: &mut x86jit_core::Vcpu, expect: u64) {
    for _ in 0..4 {
        assert_eq!(one_pass(vm, cpu), expect);
    }
}

/// A counted accumulation loop: eax = sum(1..=n). Its body block runs `n` times, so it
/// tiers up and (under background mode) switches to compiled mid-run.
fn accumulate_loop(n: i32) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut top = a.create_label();
    a.mov(eax, 0i32).unwrap();
    a.mov(ecx, n).unwrap();
    a.set_label(&mut top).unwrap();
    a.add(eax, ecx).unwrap();
    a.sub(ecx, 1i32).unwrap();
    a.jnz(top).unwrap();
    a.hlt().unwrap();
    a.assemble(CODE).unwrap()
}

/// Run to `hlt` and return RAX.
fn run_to_hlt(vm: &Vm) -> u64 {
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(vm, Some(50_000_000)), Exit::Hlt), "hlts");
    cpu.reg(Reg::Rax)
}

/// One pass through the single block (reset RIP, run to hlt), returning RAX.
fn one_pass(vm: &Vm, cpu: &mut x86jit_core::Vcpu) -> u64 {
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(vm, Some(1_000_000)), Exit::Hlt), "hlts");
    cpu.reg(Reg::Rax)
}

/// AC#1: the deterministic tier-up recipe (doc-27 D6). With threshold 3 and background
/// on, the block stays interpreted (published == 0) through the submit, `wait_idle`
/// compiles it off-thread, and the next dispatch publishes it (published == 1) — with
/// RAX identical to the interpreter throughout, no sleeps or timing.
#[test]
fn deterministic_background_tier_up() {
    let prog = single_block();
    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let vm = vm_with(Box::new(jit), 3, true, &prog);
    let mut cpu = vm.new_vcpu();

    // Four passes: run1 caches (no bump), runs 2-3 bump to 2 (< 3), run4 bumps to 3 and
    // submits the compile in the background. Still interpreted, nothing published.
    for _ in 0..4 {
        assert_eq!(one_pass(&vm, &mut cpu), 42);
    }
    assert_eq!(
        vm.cache.tier_bg_published(),
        0,
        "submitted, not yet published"
    );

    handle.wait_idle(); // the worker finishes the compile — still not published
    assert_eq!(
        vm.cache.tier_bg_published(),
        0,
        "compiled, awaiting a drain"
    );

    // The next dispatch drains the completion and publishes it.
    assert_eq!(one_pass(&vm, &mut cpu), 42);
    assert_eq!(
        vm.cache.tier_bg_published(),
        1,
        "published on the next dispatch"
    );
    assert_eq!(vm.cache.tier_bg_rejected(), 0, "no stale rejections");

    // Final state matches the interpreter oracle.
    assert_eq!(
        run_to_hlt(&vm_with(Box::new(InterpreterBackend), 3, true, &prog)),
        42
    );
}

/// AC#2: a real loop under background tier-up produces the interpreter's result and
/// actually publishes at least one background compile (the body block tiers up mid-run).
#[test]
fn real_loop_background_matches_interp_and_publishes() {
    let prog = accumulate_loop(100_000);
    let interp = run_to_hlt(&vm_with(Box::new(InterpreterBackend), 2, false, &prog));

    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let vm = vm_with(Box::new(jit), 2, true, &prog);
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(&vm, Some(50_000_000)), Exit::Hlt));
    handle.wait_idle(); // drain any straggler so the count is stable

    assert_eq!(
        cpu.reg(Reg::Rax),
        interp,
        "background result matches interp"
    );
    assert!(
        vm.cache.tier_bg_published() > 0,
        "the hot loop body tiered up in the background"
    );
}

/// BGT-6 (AC#2): with a region-forming backend AND background tier-up on, a hot loop
/// tiers up to a **background-compiled REGION** (a multi-block superblock), not just a
/// single block — off the vcpu, and only for the proven-hot loop (the eager inline
/// region path is gated off when bg is on). The result matches the interpreter.
#[test]
fn hot_loop_tiers_up_to_a_background_region() {
    const CAPS: RegionCaps = RegionCaps {
        max_blocks: 16,
        max_icount: 256,
    };
    let prog = accumulate_loop(100_000);
    let interp = run_to_hlt(&vm_with(Box::new(InterpreterBackend), 2, false, &prog));

    let jit = JitBackend::with_superblocks(CAPS);
    let handle = jit.tier_up_handle();
    let vm = vm_with(Box::new(jit), 2, true, &prog);
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(&vm, Some(50_000_000)), Exit::Hlt));
    handle.wait_idle(); // finish any straggler compile
    cpu.set_reg(Reg::Rip, CODE); // one more run drains + exercises the published region
    assert!(matches!(cpu.run(&vm, Some(50_000_000)), Exit::Hlt));

    assert_eq!(
        cpu.reg(Reg::Rax),
        interp,
        "background region matches interp"
    );
    assert!(
        vm.cache.tier_bg_published() > 0,
        "the hot loop tiered up in the background"
    );
    assert!(
        vm.cache.regions() > 0,
        "it formed a background REGION, not a single block"
    );
}

/// Build a region-forming VM with adaptive tiering: single-block tier at `t1`, region
/// tier at the higher backedge threshold `t2` (task-156).
fn vm_adaptive(backend: Box<dyn Backend>, t1: u32, t2: u32, prog: &[u8]) -> Vm {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: 0x2000 },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.set_tier_up_after(Some(t1));
    vm.set_tier_up_region_after(Some(t2));
    vm.set_tier_up_background(true);
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, prog).unwrap();
    vm
}

const ADAPT_CAPS: RegionCaps = RegionCaps {
    max_blocks: 16,
    max_icount: 256,
};

/// task-156: adaptive per-block tiering self-selects the tier — a SHORT hot loop
/// (fewer iterations than the region threshold T2) never forms a region (it would be a
/// wasted heavy compile), a LONG hot loop crosses T2 and tiers up to one. No mode
/// switch: the same VM config picks the right tier from the loop's own execution count.
#[test]
fn adaptive_tier_forms_a_region_only_for_a_long_loop() {
    const T1: u32 = 2;
    const T2: u32 = 1_000;

    // Short loop: 100 iterations < T2 → stays interpreted, no region.
    let short = accumulate_loop(100);
    let jit = JitBackend::with_superblocks(ADAPT_CAPS);
    let handle = jit.tier_up_handle();
    let vm = vm_adaptive(Box::new(jit), T1, T2, &short);
    let interp_short = run_to_hlt(&vm_with(Box::new(InterpreterBackend), 2, false, &short));
    let got = run_to_hlt(&vm);
    handle.wait_idle();
    assert_eq!(got, interp_short, "short-loop result matches interp");
    assert_eq!(
        vm.cache.regions(),
        0,
        "a short loop must NOT form a region (below T2)"
    );

    // Long loop: 100k iterations ≫ T2 → crosses the region threshold, forms a region.
    let long = accumulate_loop(100_000);
    let jit = JitBackend::with_superblocks(ADAPT_CAPS);
    let handle = jit.tier_up_handle();
    let vm = vm_adaptive(Box::new(jit), T1, T2, &long);
    let interp_long = run_to_hlt(&vm_with(Box::new(InterpreterBackend), 2, false, &long));
    let got = run_to_hlt(&vm);
    handle.wait_idle();
    assert_eq!(got, interp_long, "long-loop result matches interp");
    assert!(
        vm.cache.regions() > 0,
        "a long loop must tier up to a region (crossed T2)"
    );
}

/// AC#5: the interpreter backend with the background flag on returns `Unsupported` from
/// `tier_up_async`, so a hot block falls through to inline tier-up — behaving exactly
/// like the flag-off path (identical result, nothing published in the background).
#[test]
fn interp_backend_background_falls_back_to_inline() {
    let prog = accumulate_loop(1_000);
    let off = run_to_hlt(&vm_with(Box::new(InterpreterBackend), 2, false, &prog));
    let bg_vm = vm_with(Box::new(InterpreterBackend), 2, true, &prog);
    let on = run_to_hlt(&bg_vm);
    assert_eq!(on, off, "bg flag is a no-op on the interpreter backend");
    assert_eq!(
        bg_vm.cache.tier_bg_published(),
        0,
        "interp never publishes bg"
    );
}

// ---- BGT-4: races between a background compile and invalidation (doc-27 D5) ----

/// S1: an SMC write to the hot block's page while its compile is pending. `handle_smc`
/// (which runs before the drain in the dispatch loop) drops the block and bumps the
/// epoch, so the drain's `upgrade` rejects the now-stale compile; the block re-lifts to
/// the new bytes, re-heats, and re-tiers cleanly.
#[test]
fn smc_while_pending_rejects_then_reheats() {
    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let vm = vm_with(Box::new(jit), 3, true, &single_block_val(42));
    let mut cpu = vm.new_vcpu();

    heat_to_submit(&vm, &mut cpu, 42);
    handle.wait_idle(); // compile now sits in `done`, undrained
    assert_eq!(vm.cache.tier_bg_published(), 0);

    // Overwrite the block (marks its page dirty; invalidation lands next dispatch).
    vm.write_bytes(CODE, &single_block_val(7)).unwrap();

    assert_eq!(one_pass(&vm, &mut cpu), 7); // handle_smc → reject stale → re-lift v2
    assert_eq!(vm.cache.tier_bg_rejected(), 1, "stale compile rejected");
    assert_eq!(vm.cache.tier_bg_published(), 0);

    // New block re-tiers and publishes.
    heat_to_submit(&vm, &mut cpu, 7);
    handle.wait_idle();
    assert_eq!(one_pass(&vm, &mut cpu), 7);
    assert_eq!(vm.cache.tier_bg_published(), 1, "new block published");
    assert_eq!(vm.cache.tier_pending_len(), 0, "no stuck in-flight marker");
}

/// S2: mapping a Trap region mid-flight flushes the whole cache and bumps the epoch
/// (a JIT bakes the mmio window as a constant, so every prior compile is stale). The
/// pending compile is rejected on drain; the block re-lifts with the new window.
#[test]
fn trap_map_midflight_rejects_stale() {
    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let mut vm = vm_with_flat(Box::new(jit), 3, true, &single_block_val(42), 0x4000);
    let mut cpu = vm.new_vcpu();

    heat_to_submit(&vm, &mut cpu, 42);
    handle.wait_idle();

    // A Trap map: full flush + epoch bump (vm.rs map()).
    vm.map(0x3000, 0x1000, Prot::RW, RegionKind::Trap).unwrap();

    assert_eq!(one_pass(&vm, &mut cpu), 42);
    assert_eq!(vm.cache.tier_bg_rejected(), 1, "stale compile rejected");

    heat_to_submit(&vm, &mut cpu, 42);
    handle.wait_idle();
    assert_eq!(one_pass(&vm, &mut cpu), 42);
    assert_eq!(vm.cache.tier_bg_published(), 1);
    assert_eq!(vm.cache.tier_pending_len(), 0);
}

/// S3: an invalidation of an *unrelated* block bumps the epoch without dropping our
/// hot block. The pending compile's `upgrade` is still rejected (epoch moved), the
/// drain's `end_tier_up` frees the marker, and the surviving block resubmits and
/// publishes on the next heat.
#[test]
fn unrelated_invalidation_rejects_then_resubmits() {
    const DECOY: u64 = 0x3000;
    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let mut vm = vm_with_flat(Box::new(jit), 3, true, &single_block_val(42), 0x4000);
    vm.map(DECOY, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    let decoy = {
        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(eax, 9i32).unwrap();
        a.hlt().unwrap();
        a.assemble(DECOY).unwrap()
    };
    vm.write_bytes(DECOY, &decoy).unwrap();

    // Cache the decoy block (a victim for the later invalidation).
    let mut dcpu = vm.new_vcpu();
    dcpu.set_reg(Reg::Rip, DECOY);
    assert!(matches!(dcpu.run(&vm, Some(1_000)), Exit::Hlt));

    // Heat + submit our block, let the worker finish.
    let mut cpu = vm.new_vcpu();
    heat_to_submit(&vm, &mut cpu, 42);
    handle.wait_idle();

    // SMC the DECOY page: on the next dispatch `handle_smc` drops the decoy (a victim
    // → epoch bump) but not our block, whose in-flight marker survives.
    vm.write_bytes(DECOY, &decoy).unwrap();

    assert_eq!(one_pass(&vm, &mut cpu), 42); // drain rejects (epoch moved), block survives
    assert_eq!(vm.cache.tier_bg_rejected(), 1);
    assert_eq!(vm.cache.tier_bg_published(), 0);

    // Our block is still cached interpreted (hotness intact) → resubmits, publishes.
    assert_eq!(one_pass(&vm, &mut cpu), 42); // re-submit (hotness already ≥ threshold)
    handle.wait_idle();
    assert_eq!(one_pass(&vm, &mut cpu), 42);
    assert_eq!(vm.cache.tier_bg_published(), 1, "resubmit published");
    assert_eq!(vm.cache.tier_pending_len(), 0);
}

/// S4: two completions land for one pc (the old request still queued when an SMC
/// invalidates and the re-lifted block resubmits). The compiler is paused so both
/// requests queue; on release both compile. The epoch check rejects the stale one and
/// publishes the fresh one regardless of drain order.
#[test]
fn duplicate_completions_epoch_picks_the_fresh_one() {
    let jit = JitBackend::new();
    let handle = jit.tier_up_handle();
    let vm = vm_with(Box::new(jit), 3, true, &single_block_val(42));
    let mut cpu = vm.new_vcpu();

    let pause = handle.pause_compiler(); // stall the worker: nothing compiles yet

    // R1 (v1, epoch e0) queues but can't compile.
    heat_to_submit(&vm, &mut cpu, 42);
    assert_eq!(vm.cache.tier_pending_len(), 1);

    // SMC to v2 → next dispatch invalidates (epoch e1, clears the marker, drops the
    // block); the drain is empty (worker paused), so v2 just re-lifts.
    vm.write_bytes(CODE, &single_block_val(7)).unwrap();
    assert_eq!(one_pass(&vm, &mut cpu), 7);

    // Re-heat v2 → R2 (epoch e1) queues behind the still-uncompiled R1.
    for _ in 0..3 {
        assert_eq!(one_pass(&vm, &mut cpu), 7);
    }
    assert_eq!(vm.cache.tier_pending_len(), 1);

    // Release: the worker compiles R1 (stale) then R2 → two completions for CODE.
    drop(pause);
    handle.wait_idle();

    assert_eq!(one_pass(&vm, &mut cpu), 7);
    assert_eq!(
        vm.cache.tier_bg_published(),
        1,
        "only the fresh compile published"
    );
    assert_eq!(
        vm.cache.tier_bg_rejected(),
        1,
        "the stale duplicate rejected"
    );
    assert_eq!(vm.cache.tier_pending_len(), 0, "no stuck marker");
}
