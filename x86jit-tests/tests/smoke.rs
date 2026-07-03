//! Smoke test: the public API constructs. Real semantics tests land in M1.

use x86jit_core::{MemConsistency, MemoryModel, Vm, VmConfig};

#[test]
fn vm_constructs() {
    // Default backend = interpreter (injected internally). JIT would be injected
    // via Vm::with_backend(cfg, Box::new(JitBackend::new(..))) from x86jit-cranelift.
    let vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: 64 * 1024 },
        consistency: MemConsistency::Fast,
    });
    let _vcpu = vm.new_vcpu();
}
