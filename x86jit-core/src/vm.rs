//! `Vm` (shared) and `Vcpu` (per-thread) — the KVM-style split (§2), plus the
//! dispatcher loop (§9.2).

use std::sync::Arc;

use crate::cache::{CachedBlock, CompiledPtr, TranslationCache};
use crate::exit::{AccessKind, Exit, StepResult};
use crate::ir::IrBlock;
use crate::jit_abi::{
    call_block, MemCtx, RET_CHAIN, RET_CONTINUE, RET_EXCEPTION, RET_HLT, RET_LINK, RET_SYSCALL,
    RET_UNMAPPED,
};
use crate::lift::{lift_block, LiftError};
use crate::memory::{MapError, MemError, Memory, MemoryModel, Prot, RegionKind};
use crate::state::{CpuState, Flags, Reg};

/// Materializes IR into an executable `CachedBlock` (§8). The ONLY
/// backend-dependent operation; execution is uniform.
///
/// Injected as a trait object (§4.1) — NOT a config enum. The core can't name
/// the downstream JIT crate (dependency points the other way), so an
/// `enum Backend { Interpreter, Jit }` is unbuildable. The interpreter impl lives
/// here; `x86jit-cranelift` exports a `JitBackend` implementing this same trait
/// and the user injects it via `Vm::with_backend`.
///
/// `materialize` takes `&self` (not `&mut self`) so a `Vm` can be shared across
/// vcpus behind `Arc`. A JIT impl that needs a mutable compiler context wraps it
/// in interior mutability (e.g. `Mutex`).
pub trait Backend: Send + Sync {
    fn materialize(&self, ir: &IrBlock) -> CachedBlock;
}

/// Default backend: wrap the IR in an `Arc` and interpret it (§8.1).
pub struct InterpreterBackend;

impl Backend for InterpreterBackend {
    fn materialize(&self, ir: &IrBlock) -> CachedBlock {
        CachedBlock::Interpreted(Arc::new(ir.clone()))
    }
}

/// Memory-consistency tier for generated code on weak hosts (§4.1, §8.2.3).
/// Escalation ladder per workload: `Fast` → `AcqRel` → `FullTso`. On an x86 host
/// all tiers emit identical code (native TSO). Governs ORDINARY loads/stores only —
/// locked ops (`lock`, `xchg`) and `mfence` get real atomics/fences in every tier.
/// Distinct from `MemoryModel` (address-space layout): this is ordering.
/// Baked into compiled blocks — changing it requires flushing the translation cache.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MemConsistency {
    /// Bare STR/LDR, no barriers. Fastest. Correct only for code that doesn't
    /// synchronize through memory (single-threaded, or non-communicating threads).
    Fast,
    /// STLR / LDAPR (RCpc, ARMv8.3; LDAR fallback). The standard x86-TSO mapping;
    /// covers ~99% of correct multithreaded code (§8.2.3 theory-vs-practice note).
    AcqRel,
    /// STR+DMB ISH / LDR+DMB ISHLD. Slowest; restores store-load ordering AcqRel
    /// can miss in practice. The hammer for workloads that still misbehave.
    FullTso,
}

pub struct VmConfig {
    pub memory_model: MemoryModel,
    /// Consistency tier for weak hosts (§4.1, §8.2.3). Start: `Fast`.
    pub consistency: MemConsistency,
}

/// Shared per-machine state: guest memory + translation cache + backend (§2).
pub struct Vm {
    pub mem: Memory,
    pub cache: TranslationCache,
    pub backend: Box<dyn Backend>,
    pub consistency: MemConsistency,
}

impl Vm {
    /// Construct with the default interpreter backend (lives in the core).
    pub fn new(config: VmConfig) -> Self {
        Self::with_backend(config, Box::new(InterpreterBackend))
    }

    /// Construct with an injected backend — this is how the JIT gets in (§4.1).
    pub fn with_backend(config: VmConfig, backend: Box<dyn Backend>) -> Self {
        Self {
            mem: Memory::new(config.memory_model),
            cache: TranslationCache::new(),
            backend,
            consistency: config.consistency,
        }
    }

    pub fn map(
        &mut self,
        guest_addr: u64,
        size: usize,
        prot: Prot,
        kind: RegionKind,
    ) -> Result<(), MapError> {
        self.mem.map(guest_addr, size, prot, kind)
    }

    pub fn write_bytes(&mut self, guest_addr: u64, bytes: &[u8]) -> Result<(), MemError> {
        self.mem.write_bytes(guest_addr, bytes)
    }

    pub fn read_bytes(&self, guest_addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.mem.read_bytes(guest_addr, buf)
    }

    pub fn unmap(&mut self, guest_addr: u64, size: usize) -> Result<(), MapError> {
        self.mem.unmap(guest_addr, size)
    }

    /// Materialize a lifted block via the injected backend (§8).
    fn materialize(&self, ir: &IrBlock) -> CachedBlock {
        self.backend.materialize(ir)
    }

    /// One execution context per guest thread (§4.3). Shares this `Vm`.
    pub fn new_vcpu(&self) -> Vcpu {
        Vcpu {
            cpu: CpuState::new(),
            pending_mmio: None,
        }
    }
}

/// A value supplied by `complete_mmio_read`, waiting to be consumed by the
/// re-executed load at `addr` (§5.2). Not written into a temp (temps die on
/// block return) — matched by the retried `Load` in the memory layer.
#[derive(Copy, Clone, Debug)]
pub struct PendingMmio {
    pub addr: u64,
    pub size: u8,
    pub value: u64,
}

/// Per-guest-thread execution context: CPU state + its own `run()` loop (§2).
pub struct Vcpu {
    pub cpu: CpuState,
    /// Set by `complete_mmio_read`, consumed by the retried load (§5.2).
    pub pending_mmio: Option<PendingMmio>,
    // Breakpoints (Exit::Breakpoint) land here too once debug support exists (§14).
}

impl Vcpu {
    /// Set a guest register. GPRs route through the central index map (§3.1);
    /// RIP and the FS/GS bases live in their own `CpuState` fields (§4.3).
    /// Size-dependent GPR write semantics (32-bit zeroing) are an M1 concern in
    /// the lift's write path — this API sets the full 64-bit value.
    pub fn set_reg(&mut self, reg: Reg, val: u64) {
        match reg.gpr_index() {
            Some(i) => self.cpu.gpr[i] = val,
            None => match reg {
                Reg::Rip => self.cpu.rip = val,
                Reg::FsBase => self.cpu.fs_base = val,
                Reg::GsBase => self.cpu.gs_base = val,
                _ => unreachable!("gpr_index() only returns None for Rip/FsBase/GsBase"),
            },
        }
    }

    /// Read a guest register. Mirror of [`set_reg`]. (§4.3)
    pub fn reg(&self, reg: Reg) -> u64 {
        match reg.gpr_index() {
            Some(i) => self.cpu.gpr[i],
            None => match reg {
                Reg::Rip => self.cpu.rip,
                Reg::FsBase => self.cpu.fs_base,
                Reg::GsBase => self.cpu.gs_base,
                _ => unreachable!("gpr_index() only returns None for Rip/FsBase/GsBase"),
            },
        }
    }

    pub fn set_flags(&mut self, flags: Flags) {
        self.cpu.flags = flags;
    }

    pub fn flags(&self) -> Flags {
        self.cpu.flags
    }

    /// Deliver an MMIO read result after `Exit::MmioRead`, then resume (§5.2).
    /// Stores `(addr, size, value)` as a PENDING value; the retried load (RIP is
    /// on the faulting instruction) consumes it instead of trapping. NOT a write
    /// into a temp — temps die when the block returns (works in interp AND JIT).
    pub fn complete_mmio_read(&mut self, value: u64) {
        // The MmioRead exit carried (addr, size); store them alongside `value` so
        // the retried Load can match and consume it. Wiring in M2.
        let _ = value;
        todo!("M2: set self.pending_mmio = Some(PendingMmio{{addr,size,value}}) (§5.2)")
    }

    /// Execute until an exit event or budget exhaustion (§5.1, §9.2).
    /// `budget` is measured in blocks (§5.1 recommendation).
    ///
    /// Compiled blocks are chained (§12 M5): a direct edge whose link slot is
    /// filled hands the next entry back via `MemCtx.next_entry` and the inner loop
    /// jumps straight there, skipping the cache lookup. The budget still ticks per
    /// block, so a tight chained loop yields `BudgetExhausted` (preemption, §9.2).
    pub fn run(&mut self, vm: &Vm, budget: Option<u64>) -> Exit {
        let mut blocks_run: u64 = 0;
        let mut ctx = MemCtx::for_memory(&vm.mem);

        loop {
            if budget.is_some_and(|b| blocks_run >= b) {
                return Exit::BudgetExhausted;
            }

            let block = match resolve(vm, self.cpu.rip) {
                Ok(b) => b,
                Err(exit) => return exit,
            };

            match block {
                CachedBlock::Interpreted(ir) => {
                    match crate::interp::interpret_block(&ir, &mut self.cpu, &vm.mem) {
                        StepResult::Continue => blocks_run += 1,
                        StepResult::Exit(exit) => return exit,
                    }
                }
                CachedBlock::Compiled { entry, .. } => {
                    let mut cur = entry;
                    loop {
                        // SAFETY: `cur` is a block compiled to this ABI, alive in
                        // the JIT arena (owned by `vm`) for the call.
                        let code = unsafe { call_block(cur, &mut self.cpu, &mut ctx) };
                        blocks_run += 1;
                        match code {
                            RET_CONTINUE => break,
                            RET_CHAIN => {
                                vm.cache.record_chain();
                                cur = CompiledPtr(ctx.next_entry as *const u8);
                            }
                            RET_LINK => match resolve(vm, self.cpu.rip) {
                                Ok(CachedBlock::Compiled { entry, .. }) => {
                                    // SAFETY: `link_slot` is a live `Box<u64>` in the
                                    // JIT arena; single-threaded write (atomics at M7).
                                    unsafe { *(ctx.link_slot as *mut u64) = entry.0 as u64 };
                                    cur = entry;
                                }
                                // Mixed backend can't chain — fall back to dispatch.
                                Ok(CachedBlock::Interpreted(_)) => break,
                                Err(exit) => return exit,
                            },
                            RET_SYSCALL => return Exit::Syscall,
                            RET_HLT => return Exit::Hlt,
                            RET_UNMAPPED => return ctx.unmapped_exit(),
                            // Today only #DE (vector 0); RIP is on the faulting insn.
                            RET_EXCEPTION => {
                                return Exit::Exception { addr: self.cpu.rip, vector: 0 }
                            }
                            other => panic!("compiled block returned invalid ABI code {other}"),
                        }
                        if budget.is_some_and(|b| blocks_run >= b) {
                            return Exit::BudgetExhausted;
                        }
                    }
                }
            }
        }
    }
}

/// Fetch a block from the cache or lift+materialize it (miss). Lift errors are
/// legal exits (not `run()` failures) telling the user what to add (§9.2).
fn resolve(vm: &Vm, pc: u64) -> Result<CachedBlock, Exit> {
    if let Some(block) = vm.cache.get(pc) {
        return Ok(block);
    }
    match lift_block(&vm.mem, pc) {
        Ok(ir) => {
            let materialized = vm.materialize(&ir);
            vm.cache.insert(pc, materialized.clone());
            Ok(materialized)
        }
        Err(LiftError::Unsupported { addr, bytes, len }) => {
            Err(Exit::UnknownInstruction { addr, bytes, len })
        }
        Err(LiftError::DecodeFault { addr }) => Err(Exit::UnmappedMemory {
            addr,
            access: AccessKind::Execute,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vcpu() -> Vcpu {
        let vm = Vm::new(VmConfig {
            memory_model: MemoryModel::Flat { size: 0x1000 },
            consistency: MemConsistency::Fast,
        });
        vm.new_vcpu()
    }

    #[test]
    fn gpr_roundtrip_through_index_map() {
        let mut c = vcpu();
        c.set_reg(Reg::Rax, 0xAA);
        c.set_reg(Reg::Rbx, 0xBB);
        c.set_reg(Reg::Rsp, 0x5050);
        c.set_reg(Reg::R15, 0xF15);
        assert_eq!(c.reg(Reg::Rax), 0xAA);
        assert_eq!(c.reg(Reg::Rbx), 0xBB);
        assert_eq!(c.reg(Reg::Rsp), 0x5050);
        assert_eq!(c.reg(Reg::R15), 0xF15);
    }

    #[test]
    fn gpr_writes_land_at_encoding_order_indices() {
        let mut c = vcpu();
        c.set_reg(Reg::Rbx, 0xB); // encoding index 3, not enum position 1
        assert_eq!(c.cpu.gpr[3], 0xB);
        assert_eq!(c.cpu.gpr[1], 0); // Rcx's slot untouched
    }

    #[test]
    fn rip_and_segment_bases_use_own_fields() {
        let mut c = vcpu();
        c.set_reg(Reg::Rip, 0x400000);
        c.set_reg(Reg::FsBase, 0x7fff_0000);
        c.set_reg(Reg::GsBase, 0x7fff_1000);
        assert_eq!(c.reg(Reg::Rip), 0x400000);
        assert_eq!(c.reg(Reg::FsBase), 0x7fff_0000);
        assert_eq!(c.reg(Reg::GsBase), 0x7fff_1000);
        assert_eq!(c.cpu.rip, 0x400000);
        assert_eq!(c.cpu.fs_base, 0x7fff_0000);
        assert_eq!(c.cpu.gs_base, 0x7fff_1000);
    }
}
