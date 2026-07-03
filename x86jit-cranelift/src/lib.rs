//! Cranelift JIT backend for x86jit (§8.2).
//!
//! Compiles an [`x86jit_core::IrBlock`] to host code. Guest RAM access is
//! inlined (`host_base + guest_addr`); only Trap regions and syscalls trap out.
//!
//! Build order (§8.2.3, M4): first offsets + ABI + a "returns Continue with a
//! new RIP" block, then translate `IrOp`s one at a time, validating each
//! against the interpreter oracle.

#![cfg(feature = "jit")]

use x86jit_core::{Backend, CachedBlock, IrBlock};

/// ABI of every compiled block. All blocks share this signature so the
/// dispatcher can jump into them uniformly (§8.2.1).
///
/// - `cpu`: pointer to the guest register file (`CpuState`, `#[repr(C)]`).
/// - `mem`: pointer to the memory context (guest buffer `host_base` + metadata).
/// - returns: an encoded `StepResult`/`Exit` as a `u64` (§8.2.2).
pub type CompiledFn = unsafe extern "C" fn(cpu: *mut u8, mem: *mut u8) -> u64;

/// The JIT backend. Injected into a `Vm` via `Vm::with_backend` (§4.1) — the core
/// never names this type. Holds the executable-memory arena + Cranelift context;
/// `materialize` takes `&self`, so the mutable compiler state lives behind interior
/// mutability (a `Mutex`), keeping the backend `Send + Sync` for shared `Vm`.
pub struct JitBackend {
    // arena: ExecutableArena,          // memmap2 W^X, owned here, lives with the Vm
    // ctx: Mutex<CraneliftCtx>,        // module/builder state
}

impl JitBackend {
    pub fn new() -> Self {
        todo!("M4: init executable arena + Cranelift module")
    }
}

impl Default for JitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for JitBackend {
    fn materialize(&self, ir: &IrBlock) -> CachedBlock {
        let _entry = compile_block(ir);
        todo!("M4: wrap the compiled entry as CachedBlock::Compiled entry+guest_len")
    }
}

/// Compile a block to host code and return an entry pointer.
pub fn compile_block(_ir: &IrBlock) -> CompiledFn {
    todo!("M4: describe IrOps to a Cranelift FunctionBuilder, finalize into the code arena")
}
