//! Compiled-block ABI (§8.2.1–8.2.2) — the contract shared by the interpreter's
//! dispatcher (which runs compiled blocks) and the Cranelift backend (which emits
//! them). It lives in the core because `execute()` must run a `CachedBlock`
//! without naming the JIT crate (the dependency points the other way, §4.1).
//!
//! A compiled block has signature `fn(*mut CpuState, *mut MemCtx) -> u64`:
//! - reads/writes guest registers as fields of `*CpuState` at stable
//!   `#[repr(C)]` offsets ([`CpuOffsets`]);
//! - inlines RAM access as `MemCtx.base + guest_addr` after a bounds check;
//! - returns an encoded [`StepResult`]/[`Exit`] (`0` = Continue, see the `RET_*`
//!   codes); fault details land in `MemCtx` out-fields.

use crate::cache::CompiledPtr;
use crate::exit::{AccessKind, Exit, StepResult};
use crate::memory::Memory;
use crate::state::CpuState;

/// Every compiled block shares this signature so the dispatcher jumps in
/// uniformly (§8.2.1).
pub type CompiledFn = unsafe extern "C" fn(cpu: *mut u8, mem: *mut u8) -> u64;

// --- return codes (§8.2.2). RIP is always written by the block. ---
pub const RET_CONTINUE: u64 = 0;
pub const RET_SYSCALL: u64 = 1;
pub const RET_HLT: u64 = 2;
pub const RET_UNMAPPED: u64 = 3;
/// Block chaining (§12 M5): the block resolved its direct successor through a
/// filled link slot; `MemCtx.next_entry` holds it — the dispatcher jumps straight
/// there, skipping the cache lookup.
pub const RET_CHAIN: u64 = 4;
/// A direct edge whose link slot is still empty. `MemCtx.link_slot` holds the
/// slot address; the dispatcher compiles RIP's block and fills the slot so the
/// edge chains next time.
pub const RET_LINK: u64 = 5;
/// A guest CPU exception (today only `#DE` from div, vector 0). RIP is on the
/// faulting instruction; the dispatcher raises `Exit::Exception`.
pub const RET_EXCEPTION: u64 = 6;
/// Indirect-branch target cache miss (fast-dispatch R4): an indirect jmp/call whose
/// per-site IBTC slot was empty or held a descriptor for a *different* target.
/// `MemCtx.link_slot` holds the slot address; the dispatcher resolves RIP,
/// (re)fills the slot with an immutable `{target, entry}` descriptor unless the
/// site is megamorphic, and continues. A hit instead returns `RET_CHAIN`.
pub const RET_IBTC_MISS: u64 = 7;
/// An inlined load/store landed in a `Trap` (MMIO) region (§5.2, M4-T10). The
/// block set RIP to the faulting instruction and committed nothing of it; the
/// dispatcher single-steps that one instruction on the interpreter, which produces
/// the `MmioRead`/`MmioWrite` exit (and, on resume, consumes the pending value or
/// write-ack) before control returns to compiled code. Fault out-fields are set
/// like `RET_UNMAPPED`.
pub const RET_MMIO_DEFER: u64 = 8;

// --- MemCtx: guest memory context + fault out-params. `#[repr(C)]`; codegen
// addresses these fields by the byte offsets below. ---
#[repr(C)]
pub struct MemCtx {
    /// Host base of the guest buffer (`host_base + guest_addr` for inlined access).
    pub base: u64,
    /// Guest buffer size; a guest address `>= size` traps instead of host-UB.
    pub size: u64,
    /// Out: faulting guest address (written before returning `RET_UNMAPPED`).
    pub fault_addr: u64,
    /// Out: access width in bytes.
    pub fault_size: u64,
    /// Out: 0 = read, 1 = write.
    pub fault_access: u64,
    /// Out: next block entry pointer, set on `RET_CHAIN`.
    pub next_entry: u64,
    /// Out: address of the link slot to fill, set on `RET_LINK`.
    pub link_slot: u64,
    /// In/out: block budget for this call (§9.2, superblocks M5-T3). The dispatcher
    /// writes the remaining block quantum before each call; a compiled **region**
    /// decrements it once per guest block it enters and exits when it hits 0, so a
    /// multi-block region charges the exact same block count as the interpreter
    /// (preserving the `RunSpec::Blocks(n)` oracle). A single block never touches
    /// it, so `quantum - fuel == 0` and the dispatcher charges 1 as before.
    pub fuel: u64,
    /// In: pointer to this vcpu's [`RetStack`] shadow return stack (fast-dispatch R5).
    /// Compiled `call`s push `(return_addr, continuation_slot)` here; compiled
    /// `ret`s pop and, on a matching prediction, chain straight to the caller's
    /// continuation. Append-only ABI growth — all offsets above are unchanged, so
    /// every previously-baked block stays valid. Never null: the dispatcher points
    /// it at the vcpu's ring, and `run_compiled` at a local scratch ring.
    pub ret_stack: u64,
    /// Guest address the RAM buffer (`base`) represents at offset 0 (§4.1, identity
    /// mapping). `0` is the historical zero-based layout; non-zero means a guest
    /// address `a` maps to `base + (a - guest_base)`. The inlined RAM path bakes this
    /// as a compile-time constant (byte-identical codegen when 0); the string/x87
    /// helpers read it here to rebase their raw accesses. Append-only — all offsets
    /// above are unchanged, so previously-baked blocks stay valid.
    pub guest_base: u64,
}

/// Number of frames in the shadow return stack ring (R5). A power of two so the
/// index is a mask; wrap-and-overwrite on overflow — a lost frame only costs a
/// misprediction, never a wrong transfer (see [`RetStack`]).
pub const RET_STACK_LEN: usize = 64;

/// Per-vcpu shadow return stack (fast-dispatch R5): a fixed-size ring of
/// `(predicted_return_addr, continuation_slot_addr)` frames pushed on `call` and
/// popped on `ret`. `#[repr(C)]`; codegen addresses `sp` and `entries` by the byte
/// offsets below.
///
/// **Correctness does not depend on ring integrity.** A `ret` follows a prediction
/// only when the popped frame's `predicted_return_addr` equals the *actual* guest
/// return target AND the continuation slot holds the compiled entry that `resolve`
/// filled for that exact address. The ring only supplies a *candidate*; overflow
/// (wrap), underflow, stale frames after an epoch change, and cross-`run()` reuse
/// can each cause a missed prediction but never a wrong control transfer. No RSP
/// tracking is needed.
#[repr(C)]
pub struct RetStack {
    /// Monotonic push count; the live frame index is `sp & (RET_STACK_LEN - 1)`.
    pub sp: u64,
    /// Ring frames: `[predicted_return_addr, continuation_slot_addr]`.
    pub entries: [[u64; 2]; RET_STACK_LEN],
}

impl RetStack {
    pub fn new() -> Self {
        RetStack {
            sp: 0,
            entries: [[0; 2]; RET_STACK_LEN],
        }
    }
}

impl Default for RetStack {
    fn default() -> Self {
        Self::new()
    }
}

pub const MEMCTX_BASE: i32 = 0;
pub const MEMCTX_SIZE: i32 = 8;
pub const MEMCTX_FAULT_ADDR: i32 = 16;
pub const MEMCTX_FAULT_SIZE: i32 = 24;
pub const MEMCTX_FAULT_ACCESS: i32 = 32;
pub const MEMCTX_NEXT_ENTRY: i32 = 40;
pub const MEMCTX_LINK_SLOT: i32 = 48;
pub const MEMCTX_FUEL: i32 = 56;
pub const MEMCTX_RET_STACK: i32 = 64;
pub const MEMCTX_GUEST_BASE: i32 = 72;

// RetStack field offsets (R5): `sp` then the ring of 16-byte frames.
pub const RETSTACK_SP: i32 = 0;
pub const RETSTACK_ENTRIES: i32 = 8;
/// Byte stride between ring frames (`[u64; 2]`).
pub const RETSTACK_STRIDE: i32 = 16;

/// Byte offsets of `CpuState` fields for codegen (§8.2.1). Computed by measuring a
/// live `#[repr(C)]` value, so no unstable `offset_of!` / MSRV bump is needed —
/// the layout is a contract either way.
#[derive(Copy, Clone, Debug)]
pub struct CpuOffsets {
    pub gpr: i32,
    pub rip: i32,
    pub fs_base: i32,
    pub gs_base: i32,
    pub cf: i32,
    pub pf: i32,
    pub af: i32,
    pub zf: i32,
    pub sf: i32,
    pub of: i32,
    pub df: i32,
    pub xmm: i32,
    pub ymm_hi: i32,
    pub zmm_hi: i32,
    pub kmask: i32,
}

impl CpuOffsets {
    /// GPR slot `index` (x86 encoding order) lives at `gpr + index*8`.
    pub fn gpr(&self, index: usize) -> i32 {
        self.gpr + (index as i32) * 8
    }

    /// XMM register `index` lives at `xmm + index*16`.
    pub fn xmm(&self, index: usize) -> i32 {
        self.xmm + (index as i32) * 16
    }

    /// Upper 128 bits of YMM register `index` (task-168.2).
    pub fn ymm_hi(&self, index: usize) -> i32 {
        self.ymm_hi + (index as i32) * 16
    }

    /// Bits 511:256 of ZMM register `index`, `half` 0 = 383:256, 1 = 511:384
    /// (task-168.5). Each register occupies two contiguous 16-byte slots.
    pub fn zmm_hi(&self, index: usize, half: usize) -> i32 {
        self.zmm_hi + (index as i32) * 32 + (half as i32) * 16
    }

    /// Opmask register k`index` (k0–k7) lives at `kmask + index*8` (task-168.5).
    pub fn kmask(&self, index: usize) -> i32 {
        self.kmask + (index as i32) * 8
    }
}

/// Measure the `#[repr(C)]` field offsets of `CpuState`.
pub fn cpu_offsets() -> CpuOffsets {
    let s = CpuState::new();
    let base = &s as *const CpuState as usize;
    let off = |p: *const u8| -> i32 { (p as usize - base) as i32 };
    CpuOffsets {
        gpr: off(s.gpr.as_ptr() as *const u8),
        rip: off(&s.rip as *const u64 as *const u8),
        fs_base: off(&s.fs_base as *const u64 as *const u8),
        gs_base: off(&s.gs_base as *const u64 as *const u8),
        cf: off(&s.flags.cf as *const bool as *const u8),
        pf: off(&s.flags.pf as *const bool as *const u8),
        af: off(&s.flags.af as *const bool as *const u8),
        zf: off(&s.flags.zf as *const bool as *const u8),
        sf: off(&s.flags.sf as *const bool as *const u8),
        of: off(&s.flags.of as *const bool as *const u8),
        df: off(&s.flags.df as *const bool as *const u8),
        xmm: off(s.xmm.as_ptr() as *const u8),
        ymm_hi: off(s.ymm_hi.as_ptr() as *const u8),
        zmm_hi: off(s.zmm_hi.as_ptr() as *const u8),
        kmask: off(s.kmask.as_ptr() as *const u8),
    }
}

impl MemCtx {
    /// Build the guest-memory context for a run (fault/chain fields cleared).
    pub fn for_memory(mem: &Memory) -> Self {
        MemCtx {
            base: mem.host_base() as u64,
            size: mem.size(),
            fault_addr: 0,
            fault_size: 0,
            fault_access: 0,
            next_entry: 0,
            link_slot: 0,
            fuel: u64::MAX,
            ret_stack: 0,
            guest_base: mem.guest_base(),
        }
    }

    /// Decode an `RET_UNMAPPED` fault into the matching `Exit`.
    pub fn unmapped_exit(&self) -> Exit {
        Exit::UnmappedMemory {
            addr: self.fault_addr,
            access: if self.fault_access == 0 {
                AccessKind::Read
            } else {
                AccessKind::Write
            },
        }
    }
}

/// Call one compiled block; returns its raw ABI code (chain/link details land in
/// `ctx`). The dispatcher's chain loop (§9.2, §12 M5) interprets the code.
///
/// # Safety
/// `entry` must point at a block compiled to this exact ABI, alive in the JIT
/// arena for the call. `cpu` is exclusive; `ctx` wraps the shared guest buffer.
pub unsafe fn call_block(entry: CompiledPtr, cpu: &mut CpuState, ctx: &mut MemCtx) -> u64 {
    let f: CompiledFn = core::mem::transmute(entry.0);
    f(
        cpu as *mut CpuState as *mut u8,
        ctx as *mut MemCtx as *mut u8,
    )
}

/// Convenience: run a single compiled block and decode to a `StepResult` (used
/// where chaining isn't wired). Chain/link codes are treated as `Continue` — the
/// RIP is set either way, so the dispatcher re-resolves.
///
/// # Safety
/// As [`call_block`].
pub unsafe fn run_compiled(entry: CompiledPtr, cpu: &mut CpuState, mem: &Memory) -> StepResult {
    let mut ctx = MemCtx::for_memory(mem);
    // A block may push/pop the shadow return stack (R5); give it a live scratch ring
    // so the pointer is never null. Predictions here are inert — a single block is
    // decoded to Continue regardless — but the memory must be valid.
    let mut scratch = RetStack::new();
    ctx.ret_stack = &mut scratch as *mut RetStack as u64;
    match call_block(entry, cpu, &mut ctx) {
        RET_CONTINUE | RET_CHAIN | RET_LINK | RET_IBTC_MISS => StepResult::Continue,
        RET_SYSCALL => StepResult::Exit(Exit::Syscall),
        RET_HLT => StepResult::Exit(Exit::Hlt),
        RET_UNMAPPED => StepResult::Exit(ctx.unmapped_exit()),
        // An inlined access hit a Trap region (M4-T10): single-step the faulting
        // instruction on the interpreter, which yields the MmioRead/Write exit.
        RET_MMIO_DEFER => {
            let mut temps = Vec::new();
            crate::interp::step_one(mem, cpu, &mut temps)
        }
        // A compiled block raising a guest #DE (idiv overflow / divide-by-zero); the
        // block set RIP to the faulting instruction. Only vector 0 today.
        RET_EXCEPTION => StepResult::Exit(Exit::Exception {
            addr: cpu.rip,
            vector: 0,
        }),
        other => panic!("compiled block returned an invalid ABI code: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `MEMCTX_*` offsets are a codegen contract (the JIT bakes them). Verify
    /// them against the real `#[repr(C)]` layout so a field reorder can't silently
    /// desync the backend. `fuel` (superblocks M5-T3) must sit at 56.
    #[test]
    fn memctx_offsets_match_layout() {
        let m = MemCtx::for_memory(&Memory::new(crate::memory::MemoryModel::Flat {
            size: 0x1000,
        }));
        let base = &m as *const MemCtx as usize;
        let off = |p: *const u64| (p as usize - base) as i32;
        assert_eq!(off(&m.base), MEMCTX_BASE);
        assert_eq!(off(&m.size), MEMCTX_SIZE);
        assert_eq!(off(&m.fault_addr), MEMCTX_FAULT_ADDR);
        assert_eq!(off(&m.fault_size), MEMCTX_FAULT_SIZE);
        assert_eq!(off(&m.fault_access), MEMCTX_FAULT_ACCESS);
        assert_eq!(off(&m.next_entry), MEMCTX_NEXT_ENTRY);
        assert_eq!(off(&m.link_slot), MEMCTX_LINK_SLOT);
        assert_eq!(off(&m.fuel), MEMCTX_FUEL);
        assert_eq!(MEMCTX_FUEL, 56);
        assert_eq!(off(&m.ret_stack), MEMCTX_RET_STACK);
        assert_eq!(MEMCTX_RET_STACK, 64);
        assert_eq!(off(&m.guest_base), MEMCTX_GUEST_BASE);
        assert_eq!(MEMCTX_GUEST_BASE, 72);
    }

    /// `RetStack` field offsets are a codegen contract too (R5).
    #[test]
    fn retstack_offsets_match_layout() {
        let rs = RetStack::new();
        let base = &rs as *const RetStack as usize;
        assert_eq!(&rs.sp as *const u64 as usize - base, RETSTACK_SP as usize);
        assert_eq!(
            rs.entries.as_ptr() as usize - base,
            RETSTACK_ENTRIES as usize
        );
        // Frame stride: consecutive `[u64; 2]` entries are 16 bytes apart.
        assert_eq!(
            rs.entries[1].as_ptr() as usize - rs.entries[0].as_ptr() as usize,
            RETSTACK_STRIDE as usize
        );
    }
}
