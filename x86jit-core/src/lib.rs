//! `x86jit-core` — a guest-agnostic x86-64 recompiler engine.
//!
//! The core knows nothing about ELF, syscalls of any concrete OS, or GPUs.
//! It is fed a memory map plus an entry point and executes guest instructions,
//! yielding control through [`Exit`] whenever it hits something it does not
//! handle itself. See `wiki/design/spec.md` for the full design.
//!
//! Module map mirrors the spec's dependency order:
//! `state` + `memory` -> `ir` -> `lift` -> `interp` -> `cache`/`vm`.
//!
//! # Example
//!
//! Map a flat address space, drop in a few hand-assembled bytes, and run them on
//! the default interpreter backend:
//!
//! ```
//! use x86jit_core::{Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig};
//!
//! let mut vm = Vm::new(VmConfig {
//!     memory_model: MemoryModel::Flat { size: 0x1_0000 },
//!     consistency: MemConsistency::Fast,
//! });
//! vm.map(0, 0x1_0000, Prot::RWX, RegionKind::Ram).unwrap();
//! vm.write_bytes(0x1000, &[0xB8, 0x05, 0x00, 0x00, 0x00, 0xF4]).unwrap(); // mov eax,5 ; hlt
//!
//! let mut cpu = vm.new_vcpu();
//! cpu.set_reg(Reg::Rip, 0x1000);
//! assert!(matches!(cpu.run(&vm, None), Exit::Hlt));
//! assert_eq!(cpu.reg(Reg::Rax) as u32, 5);
//! ```
//!
//! Inject a [`Backend`] (e.g. the `x86jit-cranelift` JIT) via [`Vm::with_backend`]
//! for native-speed execution with identical guest state. See the crate's
//! `examples/` for MMIO devices and the JIT.

pub mod cache;
pub mod disasm;
pub mod exit;
pub mod f80;
pub mod interp;
pub mod ir;
pub mod jit_abi;
pub mod lift;
pub mod memory;
pub mod state;
pub mod vm;
pub mod x87;

pub use cache::{CachedBlock, CompiledPtr, TranslationCache};
pub use disasm::{disassemble, print_disassembly, DecodedInsn};
pub use exit::{AccessKind, Exit, StepResult};
pub use ir::{
    BtOp, Cond, FPrec, FlagMask, FloatBinOp, FloatUnOp, IrBlock, IrOp, IrRegion, MemOrder,
    PackedBinOp, RegionCaps, RepKind, RmwOp, StrOp, Temp, TempGen, VLogicOp, Val,
};
pub use memory::{HostRam, MapError, MemError, MemTrap, Memory, MemoryModel, Prot, RegionKind};
pub use state::{Flags, Reg};
pub use vm::{Backend, InterpreterBackend, MemConsistency, Vcpu, Vm, VmConfig};
pub use x87::FpuKind;
