//! `x86jit-core` — a guest-agnostic x86-64 recompiler engine.
//!
//! The core knows nothing about ELF, syscalls of any concrete OS, or GPUs.
//! It is fed a memory map plus an entry point and executes guest instructions,
//! yielding control through [`Exit`] whenever it hits something it does not
//! handle itself. See `wiki/design/spec.md` for the full design.
//!
//! Module map mirrors the spec's dependency order:
//! `state` + `memory` -> `ir` -> `lift` -> `interp` -> `cache`/`vm`.

pub mod cache;
pub mod disasm;
pub mod exit;
pub mod interp;
pub mod ir;
pub mod jit_abi;
pub mod lift;
pub mod memory;
pub mod state;
pub mod vm;

pub use cache::{CachedBlock, CompiledPtr, TranslationCache};
pub use disasm::{disassemble, print_disassembly, DecodedInsn};
pub use exit::{AccessKind, Exit, FaultKind, StepResult};
pub use ir::{
    Cond, FlagMask, IrBlock, IrOp, MemOrder, PackedBinOp, RepKind, StrOp, Temp, TempGen, Val,
    VLogicOp,
};
pub use memory::{MapError, MemError, MemTrap, Memory, MemoryModel, Prot, RegionKind};
pub use state::{Flags, Reg};
pub use vm::{Backend, InterpreterBackend, MemConsistency, PendingMmio, Vcpu, Vm, VmConfig};
