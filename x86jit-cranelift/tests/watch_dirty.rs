//! task-216: the Cranelift JIT inlines guest stores as raw host writes, so it must
//! feed the embedder's watched-data-range dirty tracking (`watch_range` /
//! `take_dirty_ranges`) the same way the interpreter's `Memory::note_write` does.
//! These tests run the same store under the interpreter and under a JIT-compiled block
//! (forced via `set_tier_up_after(Some(0))`, which compiles on first resolve) and assert
//! identical dirty output. Before the fix the JIT run reported nothing — the bug.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use x86jit_core::features::GuestCpuFeatures;
use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const RAM: u64 = 0x10000;
const ENTRY: u64 = 0x1000;
const TARGET: u64 = 0x4000; // watched data page, distinct from the code page

/// Assemble `code` at `ENTRY`, watch `[TARGET, TARGET+0x100)`, run to `hlt` under the
/// given backend, and return the drained dirty ranges. `regs` seeds registers before
/// each run.
///
/// For the JIT, `tier_up_after(Some(0))` compiles the block *after* its first execution,
/// so a single run would still execute interpreted. We therefore run once to warm up
/// (which compiles + swaps in the block), drain that run's dirty output, then run a
/// second time — now executing the JIT-compiled block — and return *its* dirty output.
/// That guarantees the reported ranges come from the inlined-store path, so a missing
/// hook makes the test fail (not pass trivially).
fn run_dirty(jit: bool, code: &[u8], regs: &[(Reg, u64)]) -> Vec<(u64, u64)> {
    run_dirty_features(jit, code, regs, None)
}

fn run_dirty_features(
    jit: bool,
    code: &[u8],
    regs: &[(Reg, u64)],
    features: Option<GuestCpuFeatures>,
) -> Vec<(u64, u64)> {
    let backend: Box<dyn Backend> = if jit {
        Box::new(JitBackend::new())
    } else {
        Box::new(InterpreterBackend)
    };
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), backend);
    if let Some(f) = features {
        vm.set_guest_cpu_features(f);
    }
    if jit {
        vm.set_tier_up_after(Some(0));
    }
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, code).unwrap();
    vm.watch_range(TARGET, 0x100);

    let run_once = |vm: &Vm| {
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, ENTRY);
        for &(r, v) in regs {
            cpu.set_reg(r, v);
        }
        match cpu.run(vm, None) {
            Exit::Hlt => {}
            other => panic!("unexpected exit: {other:?}"),
        }
    };

    run_once(&vm);
    if jit {
        // Discard the warmup (interpreted) run's dirty output; the block is now compiled.
        vm.take_dirty_ranges();
        run_once(&vm);
    }
    vm.take_dirty_ranges()
}

#[test]
fn jit_store_feeds_watched_dirty_ranges_like_interp() {
    // mov [rdi], eax ; hlt   — a single inlined store into the watched page.
    let code = [0x89, 0x07, 0xF4];
    let regs = [(Reg::Rdi, TARGET), (Reg::Rax, 0xdead_beef)];

    let interp = run_dirty(false, &code, &regs);
    let jit = run_dirty(true, &code, &regs);

    assert!(
        !interp.is_empty(),
        "interp must report the watched store as dirty"
    );
    assert_eq!(
        jit, interp,
        "JIT-compiled store must produce the same dirty ranges as the interpreter"
    );
    // The reported range must cover the written address.
    assert!(
        jit.iter().any(|&(a, n)| TARGET >= a && TARGET < a + n),
        "dirty range {jit:?} must cover the store at {TARGET:#x}"
    );
}

/// task-217: the multi-vCPU 0→nonzero race. A JIT'd vCPU whose run STARTED with
/// `watch_count == 0` runs a long store loop; another thread installs the first watch
/// (0→nonzero) while that loop is mid-run. The stores after the watch must show up in
/// `take_dirty_ranges` before the storing vCPU exits — which requires the JIT store gate
/// to read `watch_count` LIVE, not from a run-start snapshot. With the old snapshot gate
/// this test reports nothing (the snapshot was frozen at 0).
#[test]
fn jit_store_seen_when_watch_installed_mid_run_by_another_thread() {
    const READY: u64 = 0x5000; // an UN-watched page the guest stamps once it is running

    // mov dword [rsi], 1   ; signal "running" (READY, not watched) — runs once
    // L: mov [rdi], eax    ; store into the (soon-to-be) watched TARGET, every iteration
    //    dec ecx           ; RCX seeded by the host per run (short warmup / long main)
    //    jnz L
    //    hlt
    let code: &[u8] = &[
        0xC7, 0x06, 0x01, 0x00, 0x00, 0x00, // mov dword [rsi], 1
        0x89, 0x07, // L: mov [rdi], eax
        0xFF, 0xC9, // dec ecx
        0x75, 0xFA, // jnz L
        0xF4, // hlt
    ];

    let mut vm = Vm::with_backend(VmConfig::flat(RAM), Box::new(JitBackend::new()));
    vm.set_tier_up_after(Some(0)); // compile after the first run; the SECOND run is JIT'd
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, code).unwrap();

    let run = |vm: &Vm, iters: u64| {
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, ENTRY);
        cpu.set_reg(Reg::Rdi, TARGET);
        cpu.set_reg(Reg::Rsi, READY);
        cpu.set_reg(Reg::Rax, 0xdead_beef);
        cpu.set_reg(Reg::Rcx, iters);
        assert!(matches!(cpu.run(vm, None), Exit::Hlt));
    };

    // Warmup: a short interpreted run that COMPILES the loop block. TARGET isn't watched
    // yet, so nothing is recorded; drain to be sure.
    run(&vm, 4);
    vm.take_dirty_ranges();
    vm.write_bytes(READY, &[0u8; 4]).unwrap(); // clear the warmup's READY stamp

    // Main run: the block is now JIT-compiled, so its stores are the inlined-store path.
    // The run STARTS with TARGET unwatched (snapshot == 0). Another thread installs the
    // first watch mid-run; the live gate must still catch the post-watch JIT'd stores.
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            run(&vm, 5_000_000);
            done.store(true, Ordering::SeqCst);
        });

        // Wait until the JIT'd run is definitely underway (its run-start snapshot captured),
        // so watch_range() is a genuine mid-run 0→nonzero transition.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = [0u8; 4];
        loop {
            vm.read_bytes(READY, &mut ready).unwrap();
            if u32::from_le_bytes(ready) == 1 {
                break;
            }
            assert!(Instant::now() < deadline, "guest never signalled READY");
            std::hint::spin_loop();
        }

        // Install the first watch mid-run; give coherence a moment so the live count is
        // visible to the running vCPU while millions of loop iterations remain.
        vm.watch_range(TARGET, 0x100);
        std::thread::sleep(Duration::from_millis(5));
    });

    assert!(done.load(Ordering::SeqCst));
    let dirty = vm.take_dirty_ranges();
    assert!(
        dirty.iter().any(|&(a, n)| TARGET >= a && TARGET < a + n),
        "a JIT'd store into a range watched mid-run (0→nonzero) must be reported; \
         got {dirty:?} — the store gate is reading a stale run-start snapshot"
    );
}

/// task-273: the vector store emitters (`VStore`, `VStoreWide`, `VExtractLaneWideM`,
/// `VStoreHalf`) and the `Call` return-address push inlined raw host writes without
/// calling `note_watched_store`, so a watched range rewritten by SSE/AVX moves (a guest
/// `memcpy`, a MonoGame dynamic vertex buffer) was invisible to `take_dirty_ranges`.
/// Only the scalar/atomic/string paths were hooked — and only those were tested.
#[test]
fn jit_vector_and_call_stores_feed_watched_dirty_ranges_like_interp() {
    // Every case ends in `hlt` and writes into the watched page via RDI (or, for the
    // call, via a stack pointer parked inside it).
    // `VExtractLaneWideM` needs the EVEX form: the VEX `vextracti128` memory-destination
    // encoding is not lifted yet (lift/mod.rs: "mem dst deferred") — see the follow-up task.
    let v4 = Some(GuestCpuFeatures::v4());
    let cases: [(&str, &[u8], Option<GuestCpuFeatures>); 6] = [
        // movdqu [rdi], xmm0 — VStore, 16 bytes
        ("movdqu", &[0xF3, 0x0F, 0x7F, 0x07, 0xF4], None),
        // movd [rdi], xmm0 — VStore, 4 bytes
        ("movd", &[0x66, 0x0F, 0x7E, 0x07, 0xF4], None),
        // vmovdqu [rdi], ymm0 — VStoreWide, 32 bytes
        ("vmovdqu ymm", &[0xC5, 0xFE, 0x7F, 0x07, 0xF4], None),
        // vextracti32x4 [rdi], ymm0, 1 — VExtractLaneWideM, 16 bytes
        (
            "vextracti32x4",
            &[0x62, 0xF3, 0x7D, 0x28, 0x39, 0x07, 0x01, 0xF4],
            v4,
        ),
        // movhps [rdi], xmm0 — VStoreHalf, 8 bytes
        ("movhps", &[0x0F, 0x17, 0x07, 0xF4], None),
        // call $+5 (falls through to hlt) — the return-address push, 8 bytes
        ("call push", &[0xE8, 0x00, 0x00, 0x00, 0x00, 0xF4], None),
    ];

    for (name, code, feats) in cases {
        // The call case writes at RSP-8, so park RSP inside the watched range; the
        // others write at RDI. Seeding both keeps one helper for all cases.
        let regs = [(Reg::Rdi, TARGET), (Reg::Rsp, TARGET + 0x80)];

        let interp = run_dirty_features(false, code, &regs, feats);
        let jit = run_dirty_features(true, code, &regs, feats);

        assert!(
            !interp.is_empty(),
            "{name}: interp must report the watched store as dirty"
        );
        assert_eq!(
            jit, interp,
            "{name}: JIT-compiled store must produce the same dirty ranges as the \
             interpreter — a vector/call store path is missing note_watched_store"
        );
    }
}

#[test]
fn jit_rep_stos_feeds_watched_dirty_ranges_like_interp() {
    // rep stosb ; hlt   — a bulk string store into the watched page (AC#2).
    let code = [0xF3, 0xAA, 0xF4];
    let regs = [
        (Reg::Rdi, TARGET),
        (Reg::Rcx, 64),
        (Reg::Rax, 0x5A),
        // DF is 0 by default (forward), so RDI counts up from TARGET.
    ];

    let interp = run_dirty(false, &code, &regs);
    let jit = run_dirty(true, &code, &regs);

    assert!(!interp.is_empty(), "interp must report the rep-stos writes");
    assert!(
        !jit.is_empty(),
        "JIT rep-stos must report watched writes (string helper coverage)"
    );
    // Both must cover the 64-byte destination; the JIT path may over-approximate by up
    // to one element (documented, conservative), so require coverage, not byte-equality.
    for ranges in [&interp, &jit] {
        assert!(
            ranges
                .iter()
                .any(|&(a, n)| a <= TARGET && TARGET + 64 <= a + n),
            "dirty {ranges:?} must cover [{TARGET:#x}, {:#x})",
            TARGET + 64
        );
    }
}
