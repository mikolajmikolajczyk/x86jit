//! task-276: the Cranelift mid-end level follows the VM's tier-up policy.
//!
//! Optimizing only pays for code that runs many times. With tier-up a block reaches
//! the compiler only after proving hot, so it is worth the extra compile time; under
//! eager compilation every block is compiled on first execution and most run once,
//! where the mid-end measurably costs more than it returns (+19% to +82% on the
//! whole-program tests, and one Go server missed its startup deadline outright).
//!
//! The level is baked into the Cranelift ISA, which a `JITModule` owns one of, so the
//! backend defers building it until the first compile — by which point `Vm` has
//! reported the policy through `Backend::set_tiering`. These tests pin both halves of
//! that wiring, so an embedder writing plain `JitBackend::new()` gets the right level
//! without having to know to ask.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use x86jit_core::{
    Backend, CachedBlock, Exit, IrBlock, MemConsistency, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::{HostTarget, JitBackend, OptLevel};

const RAM: u64 = 0x10000;

/// A backend nobody has told anything to must assume its blocks may be cold.
#[test]
fn a_fresh_backend_defaults_to_unoptimized() {
    assert_eq!(JitBackend::new().opt_level(), OptLevel::None);
}

/// The derivation itself: a tiering VM upgrades a plain backend to `Speed`. Before
/// task-276's fix this stayed `None`, so an embedder that tiered got no optimization.
#[test]
fn tiering_upgrades_a_plain_backend_to_speed() {
    let jit = JitBackend::new();
    jit.set_tiering(true);
    assert_eq!(jit.opt_level(), OptLevel::Speed);
    // ...and eager takes it straight back down.
    jit.set_tiering(false);
    assert_eq!(jit.opt_level(), OptLevel::None);
}

/// An explicitly pinned level is the embedder's call and must survive the VM's
/// policy in BOTH directions — otherwise the knob would be silently overridden.
#[test]
fn an_explicit_level_outranks_the_tier_up_policy() {
    let jit = JitBackend::with_opt_level(OptLevel::SpeedAndSize);
    jit.set_tiering(false);
    assert_eq!(jit.opt_level(), OptLevel::SpeedAndSize);

    let jit = JitBackend::with_options(None, HostTarget::Native, OptLevel::None);
    jit.set_tiering(true);
    assert_eq!(jit.opt_level(), OptLevel::None);
}

/// The other half of the wiring: `Vm::set_tier_up_after` must actually report the
/// policy. Without this call the derivation above never fires and every embedder
/// silently gets the cold-code default.
#[test]
fn vm_reports_its_tier_up_policy_to_the_backend() {
    /// Counters live behind an `Arc` so the test keeps a handle after the backend is
    /// boxed into the `Vm` (which owns it and hands out no reference).
    #[derive(Clone, Default)]
    struct Log {
        calls: Arc<AtomicUsize>,
        last: Arc<AtomicUsize>, // 1 = tiered, 0 = eager
    }
    struct Spy(Log);
    impl Backend for Spy {
        fn materialize(
            &self,
            _ir: &IrBlock,
            _c: MemConsistency,
            _m: Option<(u64, u64)>,
            _g: u64,
        ) -> CachedBlock {
            unreachable!("never executed")
        }
        fn set_tiering(&self, tiered: bool) {
            self.0.calls.fetch_add(1, Ordering::Relaxed);
            self.0.last.store(tiered as usize, Ordering::Relaxed);
        }
    }

    let probe = |after: Option<u32>| -> (usize, usize) {
        let log = Log::default();
        let mut vm = Vm::with_backend(VmConfig::flat(RAM), Box::new(Spy(log.clone())));
        vm.set_tier_up_after(after);
        (
            log.calls.load(Ordering::Relaxed),
            log.last.load(Ordering::Relaxed),
        )
    };
    assert_eq!(probe(Some(4)), (1, 1), "tiering must be reported as tiered");
    assert_eq!(probe(None), (1, 0), "eager must be reported as eager");
}

/// The deferred module must still compile and run correctly once built — the lazy
/// path runs on every compile, so a mistake here would break everything.
#[test]
fn the_deferred_module_still_compiles_and_runs() {
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), Box::new(JitBackend::new()));
    vm.set_tier_up_after(Some(0)); // compile after the first execution
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    // mov eax, 7 ; hlt
    vm.write_bytes(0x1000, &[0xB8, 0x07, 0x00, 0x00, 0x00, 0xF4])
        .unwrap();

    for _ in 0..3 {
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, 0x1000);
        assert!(matches!(cpu.run(&vm, None), Exit::Hlt));
        assert_eq!(cpu.reg(Reg::Rax) & 0xffff_ffff, 7);
    }
}
