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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use x86jit_core::cache::CompiledPtr;
use x86jit_core::jit_abi::{cpu_offsets, CpuOffsets};
use x86jit_core::{Backend, CachedBlock, IrBlock, IrRegion, MemConsistency, RegionCaps};

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
    let op = [
        StrOp::Movs,
        StrOp::Stos,
        StrOp::Scas,
        StrOp::Cmps,
        StrOp::Lods,
    ][op as usize];
    let rep = [RepKind::None, RepKind::Rep, RepKind::Repe, RepKind::Repne][rep as usize];

    // Raw bounds-only view: the JIT's inlined stores skip SMC/region handling
    // (deferred, §10), so its string helper matches — no `Memory` in the ABI.
    let raw = x86jit_core::interp::RawStrMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
    };
    match x86jit_core::interp::string_run(cpu, &raw, op, elem as u8, rep, cur_addr) {
        None => RET_CONTINUE,
        Some(f) => {
            ctx.fault_addr = f.addr;
            ctx.fault_access = f.write as u64;
            RET_UNMAPPED
        }
    }
}

/// x87 helper: runs one FPU op via the shared `exec_x87`. On a memory fault it
/// writes the fault into `MemCtx`, sets RIP to the faulting instruction, and
/// returns `RET_UNMAPPED`; otherwise `RET_CONTINUE`.
///
/// # Safety
/// `cpu`/`mem` are valid pointers to a `CpuState` / `MemCtx` for the call; `kind`
/// is a valid `FpuKind` discriminant (the lift only emits real ones).
unsafe extern "C" fn x87_helper(
    cpu: *mut u8,
    mem: *mut u8,
    kind: u64,
    addr: u64,
    sti: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    // Safe: `kind` came from a real `FpuKind as u16` baked by the lift.
    let kind: x86jit_core::x87::FpuKind = std::mem::transmute(kind as u16);
    let raw = x86jit_core::x87::RawFpMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
    };
    match x86jit_core::x87::exec_x87(cpu, &raw, kind, addr, sti as u8) {
        None => RET_CONTINUE,
        Some((fault, write)) => {
            ctx.fault_addr = fault;
            ctx.fault_access = write as u64;
            cpu.rip = cur_addr;
            RET_UNMAPPED
        }
    }
}

/// fxsave/fxrstor helper: runs the 512-byte save/restore via the shared
/// `exec_fxstate`. On a memory fault it sets RIP + fault fields and returns
/// `RET_UNMAPPED`.
///
/// # Safety
/// `cpu`/`mem` are valid for the call; `mem` is a `*mut MemCtx`.
unsafe extern "C" fn fxstate_helper(
    cpu: *mut u8,
    mem: *mut u8,
    addr: u64,
    restore: u64,
    cur_addr: u64,
) -> u64 {
    use x86jit_core::jit_abi::{MemCtx, RET_CONTINUE, RET_UNMAPPED};
    let cpu = &mut *(cpu as *mut x86jit_core::state::CpuState);
    let ctx = &mut *(mem as *mut MemCtx);
    let raw = x86jit_core::x87::RawFpMem {
        base: ctx.base as *mut u8,
        size: ctx.size,
    };
    match x86jit_core::x87::exec_fxstate(cpu, &raw, addr, restore != 0) {
        None => RET_CONTINUE,
        Some((fault, write)) => {
            ctx.fault_addr = fault;
            ctx.fault_access = write as u64;
            cpu.rip = cur_addr;
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

/// `crc32` helper: CRC-32C folding via the shared `crc32c` so both backends agree.
extern "C" fn crc32_helper(crc: u64, src: u64, bytes: u64) -> u64 {
    x86jit_core::interp::crc32c(crc as u32, src, bytes as u8) as u64
}

/// The JIT backend. Injected into a `Vm` via `Vm::with_backend` (§4.1) — the core
/// never names this type. Owns the executable-memory arena (`JITModule`) and
/// Cranelift context behind a `Mutex`, so `materialize(&self)` stays `Send + Sync`
/// for a shared `Vm`.
pub struct JitBackend {
    inner: Mutex<Jit>,
    offsets: CpuOffsets,
    /// Superblock caps (§12 M5-T3), or `None` to compile one block at a time.
    caps: Option<RegionCaps>,
}

struct Jit {
    module: JITModule,
    fbctx: FunctionBuilderContext,
    next_id: u64,
    // Link slots for block chaining (§12 M5). Each `Box<AtomicU64>` holds a
    // compiled entry pointer (0 = unlinked); its heap address is baked into the
    // code and filled by the dispatcher. Owned here so it lives as long as the Vm.
    // The `Box` is load-bearing: a bare `Vec` would move its elements on growth,
    // invalidating the addresses already baked into compiled code.
    //
    // `AtomicU64` (not plain `u64`): the dispatcher fill and the SMC-driven clear
    // (`invalidate_links`, R1) both store atomically, so a vcpu reading the slot
    // from compiled code sees 0 or a valid entry, never a torn value. Compiled-code
    // loads are plain machine loads (aligned u64 is naturally atomic on the hosts
    // we target); only the Rust-side writes need the atomic type.
    #[allow(clippy::vec_box)]
    slots: Vec<Box<AtomicU64>>,
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
        builder.symbol("x86jit_x87", x87_helper as *const u8);
        builder.symbol("x86jit_fxstate", fxstate_helper as *const u8);
        builder.symbol("x86jit_crc32", crc32_helper as *const u8);
        let module = JITModule::new(builder);

        Self {
            inner: Mutex::new(Jit {
                module,
                fbctx: FunctionBuilderContext::new(),
                next_id: 0,
                slots: Vec::new(),
            }),
            offsets: cpu_offsets(),
            caps: None,
        }
    }

    /// A JIT that forms superblocks (§12 M5-T3): the dispatcher lifts a region and
    /// compiles it as one function, up to `caps`. Opt-in until M5-T3f flips the
    /// default on.
    pub fn with_superblocks(caps: RegionCaps) -> Self {
        let mut b = Self::new();
        b.caps = Some(caps);
        b
    }

    fn compile(&self, ir: &IrBlock, consistency: MemConsistency) -> CompiledPtr {
        self.compile_with(|builder, helpers, alloc_slot| {
            codegen::translate_block(builder, ir, &self.offsets, alloc_slot, helpers, consistency);
        })
    }

    /// Compile a superblock region (§12 M5-T3) as one function.
    fn compile_region(&self, region: &IrRegion, consistency: MemConsistency) -> CompiledPtr {
        self.compile_with(|builder, helpers, alloc_slot| {
            codegen::translate_region(
                builder,
                region,
                &self.offsets,
                alloc_slot,
                helpers,
                consistency,
            );
        })
    }

    /// Shared function-building spine: sets up the signature, imports the five
    /// helpers, runs `translate` to emit the body, and finalizes. `translate`
    /// receives the builder, the imported helper refs, and the link-slot allocator.
    fn compile_with(
        &self,
        translate: impl FnOnce(&mut FunctionBuilder, codegen::Helpers, &mut dyn FnMut() -> u64),
    ) -> CompiledPtr {
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

        // x87 helper: fn(cpu, mem, kind, addr, sti, cur_addr) -> i64.
        let mut x87_sig = jit.module.make_signature();
        for _ in 0..6 {
            x87_sig.params.push(AbiParam::new(types::I64));
        }
        x87_sig.returns.push(AbiParam::new(types::I64));
        let x87_id = jit
            .module
            .declare_function("x86jit_x87", Linkage::Import, &x87_sig)
            .expect("declare x87 helper");
        let x87_ref = jit.module.declare_func_in_func(x87_id, &mut ctx.func);

        // fxstate helper: fn(cpu, mem, addr, restore, cur_addr) -> i64.
        let mut fx_sig = jit.module.make_signature();
        for _ in 0..5 {
            fx_sig.params.push(AbiParam::new(types::I64));
        }
        fx_sig.returns.push(AbiParam::new(types::I64));
        let fx_id = jit
            .module
            .declare_function("x86jit_fxstate", Linkage::Import, &fx_sig)
            .expect("declare fxstate helper");
        let fx_ref = jit.module.declare_func_in_func(fx_id, &mut ctx.func);

        // crc32 helper: fn(crc, src, bytes) -> i64.
        let mut crc_sig = jit.module.make_signature();
        for _ in 0..3 {
            crc_sig.params.push(AbiParam::new(types::I64));
        }
        crc_sig.returns.push(AbiParam::new(types::I64));
        let crc_id = jit
            .module
            .declare_function("x86jit_crc32", Linkage::Import, &crc_sig)
            .expect("declare crc32 helper");
        let crc_ref = jit.module.declare_func_in_func(crc_id, &mut ctx.func);

        {
            let Jit { fbctx, slots, .. } = &mut *jit;
            let mut alloc_slot = || {
                let b = Box::new(AtomicU64::new(0));
                let addr = &*b as *const AtomicU64 as u64;
                slots.push(b);
                addr
            };
            let mut builder = FunctionBuilder::new(&mut ctx.func, fbctx);
            let helpers = codegen::Helpers {
                div: div_ref,
                string: str_ref,
                cpuid: cpuid_ref,
                x87: x87_ref,
                fxstate: fx_ref,
                crc32: crc_ref,
            };
            translate(&mut builder, helpers, &mut alloc_slot);
            builder.finalize();
        }

        let id = jit
            .module
            .declare_function(&name, Linkage::Export, &ctx.func.signature)
            .expect("declare function");
        jit.module
            .define_function(id, &mut ctx)
            .expect("define function");
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
    fn materialize(&self, ir: &IrBlock, consistency: MemConsistency) -> CachedBlock {
        CachedBlock::Compiled {
            entry: self.compile(ir, consistency),
            guest_len: ir.guest_len,
        }
    }

    fn region_caps(&self) -> Option<RegionCaps> {
        self.caps
    }

    fn materialize_region(&self, region: &IrRegion, consistency: MemConsistency) -> CachedBlock {
        // `guest_len` on the cached unit is vestigial (SMC uses the span list); use
        // the entry block's length.
        CachedBlock::Compiled {
            entry: self.compile_region(region, consistency),
            guest_len: region.blocks[0].guest_len,
        }
    }

    fn invalidate_links(&self) {
        // Zero every link slot so no surviving block chains into a unit an SMC
        // invalidation just dropped (R1). Over-invalidation (all slots, not only
        // the victims') is deliberate: invalidation is rare, and a cleared slot
        // simply re-links via `RET_LINK` on its next traversal. Relaxed stores pair
        // with the dispatcher's relaxed fill; compiled-code reads see 0 or a valid
        // entry. Runs under the compiler mutex, off the hot path.
        let jit = self.inner.lock().unwrap();
        for slot in &jit.slots {
            slot.store(0, Ordering::Relaxed);
        }
    }
}
