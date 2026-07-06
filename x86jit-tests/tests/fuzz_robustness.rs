//! Robustness fuzzing (production hardening): feed **random bytes** as guest code
//! with random register state and assert the engine always returns an `Exit` — a
//! guest must never panic or UB the host, whatever it decodes to. Unlike the
//! differential fuzzer (which builds *valid* programs), this hammers the decode /
//! lift / interpreter / JIT paths with adversarial input.

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x10_0000;
const CODE: u64 = 0x1000;
const STACK: u64 = 0x8000; // a valid mid-buffer stack so push/pop/call land in RAM

/// Deterministic xorshift64 — no external RNG, reproducible per seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// Run `len` random bytes as code under `make_backend`, with random registers and a
/// block budget. Returns the `Exit` — the point is that it *returns* (no panic/hang).
fn run_random(seed: u64, len: usize, make_backend: impl Fn() -> Box<dyn Backend>) -> Exit {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1));
    let code: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();

    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        make_backend(),
    );
    // One big RWX-ish RAM region: prot isn't enforced (§4.2), so this backs execute,
    // load, store, and self-modifying writes — the widest attack surface.
    vm.map(0, FLAT as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rsp, STACK);
    // Random GPRs so memory operands / shift counts / divisors hit edge values.
    for r in [
        Reg::Rax,
        Reg::Rbx,
        Reg::Rcx,
        Reg::Rdx,
        Reg::Rsi,
        Reg::Rdi,
        Reg::Rbp,
    ] {
        cpu.set_reg(r, rng.next());
    }

    // A block budget bounds any infinite guest loop; re-enter a few rounds so block
    // chaining across runs gets exercised. Any non-budget exit (fault/syscall/MMIO/
    // unknown insn) would be handled by an embedder — for the fuzzer just stop there.
    for _ in 0..8 {
        match cpu.run(&vm, Some(64)) {
            Exit::BudgetExhausted => continue,
            other => return other,
        }
    }
    Exit::BudgetExhausted
}

#[test]
fn random_bytes_never_panic_interp() {
    // The interpreter is fast, so hammer it hard.
    for seed in 0..30_000u64 {
        let len = 1 + (seed as usize % 15); // 1..15 bytes — full x86 insn length range
        let _ = run_random(seed, len, || Box::new(InterpreterBackend));
    }
}

#[test]
fn random_bytes_never_panic_jit() {
    // The JIT compiles each block, so fewer seeds — still exercises codegen on
    // adversarial input.
    for seed in 0..1_500u64 {
        let len = 1 + (seed as usize % 15);
        let _ = run_random(seed, len, || Box::new(JitBackend::new()));
    }
}

/// Longer random blobs (up to 48 bytes) exercise multi-instruction blocks, chaining,
/// and the superblock JIT's region formation with adversarial control flow — the
/// path that walks the guest CFG (`lift_region` / reverse-post-order removal).
#[test]
fn longer_random_blobs_never_panic() {
    let caps = x86jit_core::RegionCaps {
        max_blocks: 16,
        max_icount: 256,
    };
    for seed in 0..2_000u64 {
        let len = 1 + (seed as usize % 48);
        let _ = run_random(seed, len, || Box::new(InterpreterBackend));
        if seed % 8 == 0 {
            let _ = run_random(seed, len, || Box::new(JitBackend::new()));
            let _ = run_random(seed, len, move || {
                Box::new(JitBackend::with_superblocks(caps))
            });
        }
    }
}
