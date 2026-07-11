//! task-216: the Cranelift JIT inlines guest stores as raw host writes, so it must
//! feed the embedder's watched-data-range dirty tracking (`watch_range` /
//! `take_dirty_ranges`) the same way the interpreter's `Memory::note_write` does.
//! These tests run the same store under the interpreter and under a JIT-compiled block
//! (forced via `set_tier_up_after(Some(0))`, which compiles on first resolve) and assert
//! identical dirty output. Before the fix the JIT run reported nothing — the bug.

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
    let backend: Box<dyn Backend> = if jit {
        Box::new(JitBackend::new())
    } else {
        Box::new(InterpreterBackend)
    };
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), backend);
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
