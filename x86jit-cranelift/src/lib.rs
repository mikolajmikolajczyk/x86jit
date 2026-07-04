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

/// Division helper called from compiled code (div isn't hot, so a call is fine and
/// avoids 128-bit codegen). Reuses the interpreter's `divide` so both agree.
/// `out` points at `[quot, rem]`; returns 0 on success, 1 on `#DE`.
///
/// # Safety
/// `out` must point at two writable `u64`s. Called only from JIT code with a valid
/// stack-slot pointer.
unsafe extern "C" fn div_helper(
    hi: u64,
    lo: u64,
    divisor: u64,
    size: u64,
    signed: u64,
    out: *mut u64,
) -> u64 {
    match x86jit_core::interp::divide(hi, lo, divisor, size as u8, signed != 0) {
        Some((q, r)) => {
            *out = q;
            *out.add(1) = r;
            0
        }
        None => 1,
    }
}

/// String-op helper: runs the whole (rep) loop via the shared `string_run`. Reads
/// `cpu` and the guest buffer (`MemCtx.base`/`size`); on a trap it writes the
/// fault into `MemCtx` and returns `RET_UNMAPPED`, else `RET_CONTINUE`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call.
unsafe extern "C" fn string_helper(
    cpu: *mut u8,
    mem: *mut u8,
    op: u64,
    elem: u64,
    rep: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    use x86jit_core::{RepKind, StrOp};

    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let op = [StrOp::Movs, StrOp::Stos, StrOp::Scas, StrOp::Cmps, StrOp::Lods][op as usize];
    let rep = [RepKind::None, RepKind::Rep, RepKind::Repe, RepKind::Repne][rep as usize];

    match x86jit_core::interp::string_run(cpu, ctx.base as *mut u8, ctx.size, op, elem as u8, rep, cur_addr) {
        None => RET_CONTINUE,
        Some((addr, write)) => {
            ctx.fault_addr = addr;
            ctx.fault_access = write as u64;
            RET_UNMAPPED
        }
    }
}

/// `cpuid` helper: delegates to the shared `cpuid_run` so both backends report the
/// same features.
///
/// # Safety
/// `cpu` is a valid pointer to a `CpuState` for the call.
unsafe extern "C" fn cpuid_helper(cpu: *mut u8) {
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    x86jit_core::interp::cpuid_run(cpu);
}

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
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbol("x86jit_div", div_helper as *const u8);
        builder.symbol("x86jit_string", string_helper as *const u8);
        builder.symbol("x86jit_cpuid", cpuid_helper as *const u8);
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

        // Import the div helper into this function.
        let mut div_sig = jit.module.make_signature();
        for _ in 0..6 {
            div_sig.params.push(AbiParam::new(types::I64));
        }
        div_sig.returns.push(AbiParam::new(types::I64));
        let div_id = jit
            .module
            .declare_function("x86jit_div", Linkage::Import, &div_sig)
            .expect("declare div helper");
        let div_ref = jit.module.declare_func_in_func(div_id, &mut ctx.func);

        // String helper: fn(cpu, mem, op, elem, rep, cur_addr) -> i64.
        let mut str_sig = jit.module.make_signature();
        for _ in 0..6 {
            str_sig.params.push(AbiParam::new(types::I64));
        }
        str_sig.returns.push(AbiParam::new(types::I64));
        let str_id = jit
            .module
            .declare_function("x86jit_string", Linkage::Import, &str_sig)
            .expect("declare string helper");
        let str_ref = jit.module.declare_func_in_func(str_id, &mut ctx.func);

        // cpuid helper: fn(cpu) -> ().
        let mut cpuid_sig = jit.module.make_signature();
        cpuid_sig.params.push(AbiParam::new(types::I64));
        let cpuid_id = jit
            .module
            .declare_function("x86jit_cpuid", Linkage::Import, &cpuid_sig)
            .expect("declare cpuid helper");
        let cpuid_ref = jit.module.declare_func_in_func(cpuid_id, &mut ctx.func);

        {
            let Jit { fbctx, slots, .. } = &mut *jit;
            let mut alloc_slot = || {
                let b = Box::new(0u64);
                let addr = &*b as *const u64 as u64;
                slots.push(b);
                addr
            };
            let mut builder = FunctionBuilder::new(&mut ctx.func, fbctx);
            let helpers = codegen::Helpers { div: div_ref, string: str_ref, cpuid: cpuid_ref };
            codegen::translate_block(&mut builder, ir, &self.offsets, &mut alloc_slot, helpers);
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
