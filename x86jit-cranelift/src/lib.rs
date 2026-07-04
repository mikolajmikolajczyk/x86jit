//! Cranelift JIT backend for x86jit (§8.2).
//!
//! Compiles an [`x86jit_core::IrBlock`] to host code. Guest RAM access is inlined
//! (`host_base + guest_addr` after a bounds check); only out-of-range access and
//! syscalls trap out. The compiled-block ABI (signature, result encoding, field
//! offsets) is defined once in `x86jit_core::jit_abi` and shared with the
//! dispatcher; this crate only emits code matching it.
//!
//! Build order (§8.2.3): offsets + ABI + a "returns Continue" block first, then
//! `IrOp`s one at a time, each validated against the interpreter oracle.

#![cfg(feature = "jit")]

mod codegen;

use std::sync::Mutex;

use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use x86jit_core::cache::CompiledPtr;
use x86jit_core::jit_abi::{cpu_offsets, CpuOffsets};
use x86jit_core::{Backend, CachedBlock, IrBlock};

/// The JIT backend. Injected into a `Vm` via `Vm::with_backend` (§4.1) — the core
/// never names this type. Owns the executable-memory arena (`JITModule`) and
/// Cranelift context behind a `Mutex`, so `materialize(&self)` stays `Send + Sync`
/// for a shared `Vm`.
pub struct JitBackend {
    inner: Mutex<Jit>,
    offsets: CpuOffsets,
}

struct Jit {
    module: JITModule,
    fbctx: FunctionBuilderContext,
    next_id: u64,
    // Link slots for block chaining (§12 M5). Each `Box<u64>` holds a compiled
    // entry pointer (0 = unlinked); its heap address is baked into the code and
    // filled by the dispatcher. Owned here so it lives as long as the Vm. The
    // `Box` is load-bearing: a bare `Vec<u64>` would move its elements on growth,
    // invalidating the addresses already baked into compiled code.
    #[allow(clippy::vec_box)]
    slots: Vec<Box<u64>>,
}

impl JitBackend {
    pub fn new() -> Self {
        let mut flags = settings::builder();
        flags.set("use_colocated_libcalls", "false").unwrap();
        flags.set("is_pic", "false").unwrap();
        let isa = cranelift_native::builder()
            .expect("host ISA")
            .finish(settings::Flags::new(flags))
            .expect("finish ISA");
        let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        let module = JITModule::new(builder);

        Self {
            inner: Mutex::new(Jit {
                module,
                fbctx: FunctionBuilderContext::new(),
                next_id: 0,
                slots: Vec::new(),
            }),
            offsets: cpu_offsets(),
        }
    }

    fn compile(&self, ir: &IrBlock) -> CompiledPtr {
        let mut jit = self.inner.lock().unwrap();
        jit.next_id += 1;
        let name = format!("blk_{}", jit.next_id);

        let mut ctx = jit.module.make_context();
        let ptr = jit.module.target_config().pointer_type();
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.params.push(AbiParam::new(ptr));
        ctx.func.signature.returns.push(AbiParam::new(types::I64));

        {
            let Jit { fbctx, slots, .. } = &mut *jit;
            let mut alloc_slot = || {
                let b = Box::new(0u64);
                let addr = &*b as *const u64 as u64;
                slots.push(b);
                addr
            };
            let mut builder = FunctionBuilder::new(&mut ctx.func, fbctx);
            codegen::translate_block(&mut builder, ir, &self.offsets, &mut alloc_slot);
            builder.finalize();
        }

        let id = jit
            .module
            .declare_function(&name, Linkage::Export, &ctx.func.signature)
            .expect("declare function");
        jit.module.define_function(id, &mut ctx).expect("define function");
        jit.module.clear_context(&mut ctx);
        jit.module.finalize_definitions().expect("finalize");

        CompiledPtr(jit.module.get_finalized_function(id))
    }
}

impl Default for JitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for JitBackend {
    fn materialize(&self, ir: &IrBlock) -> CachedBlock {
        CachedBlock::Compiled {
            entry: self.compile(ir),
            guest_len: ir.guest_len,
        }
    }
}
