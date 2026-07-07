//! Guard-page fault recovery (doc-30 GP-2): a host SIGSEGV raised by JIT'd guest code
//! touching a `PROT_NONE` guard page (an in-span-but-unmapped access — e.g. a Go
//! nil-deref, page 0 of the huge `Reserved` span) is converted into a resumable
//! `Exit::UnmappedMemory`, matching the trap the interpreter already produces. Closes
//! decision-3.
//!
//! Mechanism: [`guarded_run`] wraps `Vcpu::run` in a `sigsetjmp`; a `SA_SIGINFO`
//! SIGSEGV handler, on a fault whose address lands inside the guest span while a guard
//! is armed on this thread, records the fault and `siglongjmp`s back — unwinding the
//! (destructor-free) JIT frames to `guarded_run`, which returns the `Exit`. Any other
//! fault (no armed guard, or an address outside the guest span — a genuine host bug,
//! incl. the JIT arena's W^X) restores the previous disposition and re-fires, so the
//! process still crashes honestly with its core dump.
//!
//! **glibc host assumption.** `sigsetjmp` is a C macro, so we bind glibc's
//! `__sigsetjmp`. The x86jit host toolchain is glibc (nix devShell + CI); a musl host
//! would need a small C shim instead. The guest may be musl — that's unrelated (this
//! is host-side).

use std::cell::UnsafeCell;
use std::ffi::c_int;
use std::ptr;
use std::sync::Once;

use x86jit_core::{AccessKind, Exit, Reg, Vcpu, Vm};

// --- sigsetjmp / siglongjmp (glibc; sigsetjmp is a macro → __sigsetjmp) ---

/// Opaque `sigjmp_buf`, over-sized past any arch's real size (x86-64 ~200 B, aarch64
/// ~312 B) so `__sigsetjmp`/`siglongjmp` only ever write within it. 16-aligned.
#[repr(C, align(16))]
struct JmpBuf([u64; 64]);

extern "C" {
    fn __sigsetjmp(env: *mut JmpBuf, savemask: c_int) -> c_int;
    fn siglongjmp(env: *mut JmpBuf, val: c_int) -> !;
}

/// Per-host-thread guard state. Published by [`guarded_run`] before entering the vcpu,
/// read (and the fault fields written) by the signal handler on the same thread. Never
/// touched concurrently — a thread is either running the vcpu or in its own handler.
struct GuardSlot {
    /// A `sigsetjmp` is armed and we're inside `Vcpu::run` — the handler may convert.
    active: bool,
    jmp: JmpBuf,
    /// Guest span `[base, base+size)` in host address terms (`host_base()`/`size()`).
    mem_base: u64,
    mem_size: u64,
    /// Written by the handler before the longjmp.
    fault_addr: u64,
    fault_write: bool,
    /// Faulting host program counter — resolved to a precise guest RIP via the
    /// `CodeMap` srcloc side table after the longjmp (GP-3).
    fault_pc: u64,
}

thread_local! {
    static SLOT: UnsafeCell<GuardSlot> = const { UnsafeCell::new(GuardSlot {
        active: false,
        jmp: JmpBuf([0; 64]),
        mem_base: 0,
        mem_size: 0,
        fault_addr: 0,
        fault_write: false,
        fault_pc: 0,
    }) };
}

// The previous SIGSEGV disposition, saved at install and restored by the handler when a
// fault is NOT a recoverable guest fault (so a real crash keeps its core dump). Written
// once under `INSTALL`, read only in the handler.
static INSTALL: Once = Once::new();
struct OldSa(UnsafeCell<libc::sigaction>);
unsafe impl Sync for OldSa {}
static OLD: OldSa = OldSa(UnsafeCell::new(unsafe { std::mem::zeroed() }));

/// The faulting address from a SIGSEGV `siginfo_t`. libc doesn't expose `si_addr` for
/// linux-gnu, so read it from the `_sigfault` union member by its `#[repr(C)]` prefix:
/// `si_addr` follows `si_signo/si_errno/si_code` (three `int`s, padded to an 8-byte
/// boundary on 64-bit) → offset 16.
#[repr(C)]
struct SigfaultPrefix {
    _si_signo: c_int,
    _si_errno: c_int,
    _si_code: c_int,
    _pad: c_int,
    si_addr: *mut libc::c_void,
}

unsafe fn fault_addr(info: *const libc::siginfo_t) -> u64 {
    unsafe { (*(info as *const SigfaultPrefix)).si_addr as u64 }
}

/// Faulting host program counter from the trap context (D4 platform seam). Used
/// to resolve the precise guest RIP via the `CodeMap` (GP-3). `0` → unknown.
#[cfg(target_arch = "x86_64")]
unsafe fn fault_pc(uc: *const libc::c_void) -> u64 {
    let uc = uc as *const libc::ucontext_t;
    unsafe { (*uc).uc_mcontext.gregs[libc::REG_RIP as usize] as u64 }
}

#[cfg(target_arch = "aarch64")]
unsafe fn fault_pc(uc: *const libc::c_void) -> u64 {
    let uc = uc as *const libc::ucontext_t;
    unsafe { (*uc).uc_mcontext.pc }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
unsafe fn fault_pc(_uc: *const libc::c_void) -> u64 {
    0
}

/// Per-arch access-direction extraction from the trap context. `None` → unknown (the
/// caller defaults to `Read`). This is the only host-arch-specific surface (D4).
#[cfg(target_arch = "x86_64")]
unsafe fn is_write(uc: *const libc::c_void) -> Option<bool> {
    // x86-64: page-fault error code in `gregs[REG_ERR]`, bit 1 = write.
    let uc = uc as *const libc::ucontext_t;
    let err = unsafe { (*uc).uc_mcontext.gregs[libc::REG_ERR as usize] };
    Some(err & 0x2 != 0)
}

#[cfg(target_arch = "aarch64")]
unsafe fn is_write(_uc: *const libc::c_void) -> Option<bool> {
    // aarch64: the write bit lives in ESR_ELx.WnR inside the `__reserved` esr_context;
    // parsing it is deferred — default to Read (a nil-deref, the common case, reads).
    None
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
unsafe fn is_write(_uc: *const libc::c_void) -> Option<bool> {
    None
}

extern "C" fn handler(_sig: c_int, info: *mut libc::siginfo_t, uc: *mut libc::c_void) {
    // SAFETY: signal context; we only touch this thread's TLS slot (no alloc, no lock)
    // and, on the non-guest path, async-signal-safe `sigaction`.
    let addr = unsafe { fault_addr(info) };
    let slot = SLOT.with(|s| s.get());
    let is_guest = unsafe {
        (*slot).active && addr >= (*slot).mem_base && addr < (*slot).mem_base + (*slot).mem_size
    };
    if is_guest {
        unsafe {
            (*slot).fault_addr = addr - (*slot).mem_base;
            (*slot).fault_write = is_write(uc).unwrap_or(false);
            (*slot).fault_pc = fault_pc(uc);
            siglongjmp(ptr::addr_of_mut!((*slot).jmp), 1);
        }
    }
    // Not a recoverable guest fault (no armed guard, or a host-bug address outside the
    // span — including a JIT-arena W^X fault). Restore the previous disposition and
    // return: the instruction re-executes and faults again under it (SIG_DFL → the
    // honest core dump; an embedder's own handler runs).
    unsafe {
        libc::sigaction(libc::SIGSEGV, OLD.0.get(), ptr::null_mut());
    }
}

fn install() {
    INSTALL.call_once(|| unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGSEGV, &sa, OLD.0.get());
    });
}

/// Run `cpu` under a guard: a JIT (or interpreter) access to a `PROT_NONE` guard page
/// of `vm`'s host-backed span returns `Exit::UnmappedMemory { addr, access }` (with
/// `addr` the guest offset) instead of crashing the host. Behaves exactly like
/// `cpu.run(vm, budget)` when no guard fault occurs. On a memory model with no guard
/// pages (a `Vec` backing) it is a pure wrapper — the fault path simply never fires.
pub fn guarded_run(cpu: &mut Vcpu, vm: &Vm, budget: Option<u64>) -> Exit {
    install();
    // Raw TLS pointer held in THIS frame — `__sigsetjmp` must run in the frame the
    // handler will `siglongjmp` back to, so it cannot sit inside a `.with()` closure
    // (that frame would be dead by the time the fault fires).
    let slot = SLOT.with(|s| s.get());
    unsafe {
        (*slot).mem_base = vm.mem.host_base() as u64;
        (*slot).mem_size = vm.mem.size();
        // Returns 0 on the initial call, or 1 when the handler longjmps back on a fault.
        if __sigsetjmp(ptr::addr_of_mut!((*slot).jmp), 1) != 0 {
            (*slot).active = false;
            let access = if (*slot).fault_write {
                AccessKind::Write
            } else {
                AccessKind::Read
            };
            // GP-3: recover the precise faulting guest RIP from the JIT srcloc
            // side table. Only JIT faults reach this longjmp (the interpreter
            // traps in-band without a signal); mid-block the JIT hasn't stored
            // RIP, so without this it would be stale. On a miss (PC in no
            // registered range) leave RIP as-is.
            if let Some(rip) = x86jit_core::codemap::lookup((*slot).fault_pc as usize) {
                cpu.set_reg(Reg::Rip, rip);
            }
            return Exit::UnmappedMemory {
                addr: (*slot).fault_addr,
                access,
            };
        }
        (*slot).active = true;
    }
    let exit = cpu.run(vm, budget);
    unsafe {
        (*slot).active = false;
    }
    exit
}
