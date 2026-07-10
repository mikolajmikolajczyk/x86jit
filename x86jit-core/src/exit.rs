//! Return-based exit reasons and the backend execution result (§5.2, §8).

/// Direction of a memory access that could not complete inline.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AccessKind {
    Read,
    Write,
    Execute,
}

/// Reason `run()` returned control to the user (§5.2).
#[derive(Clone, Debug)]
pub enum Exit {
    /// Guest executed syscall/sysenter/int 0x80. Args are in guest registers.
    Syscall,
    /// Guest executed `hlt`.
    Hlt,
    /// Access to an unmapped address.
    UnmappedMemory { addr: u64, access: AccessKind },
    /// READ from a Trap region (MMIO). Guest waits; user must call
    /// `complete_mmio_read` before the next `run()`.
    MmioRead { addr: u64, size: u8 },
    /// WRITE to a Trap region (MMIO). User handles the side effect, then resumes.
    MmioWrite { addr: u64, size: u8, value: u64 },
    /// An instruction the lift does not yet support — tells you what to add next.
    UnknownInstruction { addr: u64, bytes: [u8; 15], len: u8 },
    /// A guest CPU exception, NOT a lift failure: `#DE` divide-by-zero, `#UD`
    /// (`ud2`), `#BP` (`int3`), `#DB` (`int1`), etc. HLE maps these to
    /// SIGFPE/SIGILL/SIGTRAP. `vector` = x86 exception vector (§14 open decision).
    /// `addr` is the guest **saved RIP** — the x86 fault/trap convention: a fault
    /// (`#DE`/`#UD`) leaves it on the faulting instruction, a trap (`#BP`/`#DB`)
    /// resumes past it. It always equals the vcpu's RIP at exit.
    Exception { addr: u64, vector: u8 },
    /// `budget` blocks executed — cooperative yield.
    BudgetExhausted,
}

/// Result of executing one materialized block. Distinguishes "keep going" from
/// "trap out" so the dispatcher knows whether to continue or return (§8).
pub enum StepResult {
    /// Block finished; new RIP already written to `CpuState`. Continue.
    Continue,
    /// Block trapped out to the user. Execution stops.
    Exit(Exit),
}
