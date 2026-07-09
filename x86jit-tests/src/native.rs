//! `NativeOracle` (testing.md §4) — execute the guest snippet on the **real host
//! CPU** and read the final architectural state back. On an x86-64 host this is the
//! fastest, most faithful oracle, and — crucially — the only one that can oracle
//! **VEX/EVEX** instructions: Unicorn's QEMU build drops `VEX.vvvv`, so it silently
//! mis-decodes BMI/AVX (the bzhi/pdep/pext/shld divergences chased down in task-185
//! were *Unicorn* bugs, not ours). The real CPU has no such blind spot.
//!
//! ## How it runs guest code in-process, safely
//!
//! The snippet ends in `hlt` — privileged, so it faults (`#GP` → `SIGSEGV`) in user
//! mode (testing.md §2 caveat). Rather than a fragile in-process recovery, each run
//! **`fork`s a child**:
//!
//! 1. Parent maps the guest memory at its guest VAs plus three fixed control pages,
//!    fills an input block, assembles a tiny register-load **stub**, and forks.
//! 2. Child installs a `SIGSEGV`/`SIGILL`/… handler on a `sigaltstack` (the guest runs
//!    with its own `RSP`, so the signal frame can't live on the guest stack), then
//!    jumps into the stub, which loads the guest GPRs/flags/XMM and jumps to `entry`.
//!    The handler is armed *in the child*, after `fork`: doing it process-wide in the
//!    parent would displace Rust's own fatal-signal reporters (a genuine `SIGSEGV`
//!    elsewhere in the test process would be laundered into a clean `_exit(0)`).
//!    `sigaction` is async-signal-safe, so arming it between `fork` and the jump is legal.
//! 3. The guest runs on the bare CPU and hits `hlt` → `SIGSEGV`. The handler snapshots
//!    the register file from the signal `ucontext` into a **shared** page and `_exit`s.
//! 4. Parent `waitpid`s, reads the shared page, reads back guest memory, unmaps.
//!
//! `fork` gives free crash-isolation: a wild guest (bad jump, unmapped access,
//! unsupported instruction) kills only the child, and the parent reports "couldn't
//! run this natively" (`None`) instead of dying. A non-`hlt` fault (e.g. an EVEX op
//! on a host without AVX-512 → `SIGILL`) likewise degrades to `None`, so the caller
//! simply skips that input rather than seeing a bogus divergence.
//!
//! Captures GPRs, RIP, RFLAGS, the 16 XMM registers, and — on an AVX host (task-191) —
//! the YMM upper halves, read from the extended XSAVE area of the signal frame. The stub
//! `vzeroall`s first so an untouched YMM upper reads back zero (matching the
//! interpreter's zero-init), not the child's inherited-dirty FPU state. ZMM upper halves
//! and the opmask (`k`) registers are not yet captured — the test harness `CpuSnapshot`
//! tops out at YMM, so they have nowhere to be compared until it grows those fields
//! (that harness extension lands with the AVX-512 lift work).

use iced_x86::code_asm::*;

use crate::oracle::{RunOutcome, VectorInput};
use crate::vector::{CpuSnapshot, ExitKind, MemChunk, RunSpec, SnapFlags};

const PAGE: u64 = 0x1000;

// Fixed low control window, reachable by 32-bit absolute displacements from the
// stub (< 2 GiB). Distinct pages so each has its own protection/sharing. These must
// not overlap any guest chunk — the fuzzer places its guest at 0x210000+ (fuzz.rs).
const CTRL: u64 = 0x0020_0000; // input block: guest GPRs/flags/XMM/entry (parent → child)
const SHARE: u64 = 0x0020_1000; // capture block: final state (child handler → parent)
const STUB: u64 = 0x0020_2000; // the register-load + jump-to-guest stub

// Input-block field offsets (must match what the stub bakes as absolute addresses).
const IN_GPR: u64 = 0; //   [u64; 16], x86 encoding order
const IN_RFLAGS: u64 = 128; // u64
const IN_ENTRY: u64 = 136; // u64 (guest RIP)
const IN_XMM: u64 = 144; //  [u128; 16], 16-byte aligned
const IN_YMM_OFFSET: u64 = 400; // u32: byte offset of the YMM component in the signal
                                // XSAVE area (0 = no AVX → skip YMM capture)

/// Byte offset of the `_fpx_sw_bytes` block inside the 512-byte legacy FXSAVE area of a
/// signal `fpstate` — its `magic1` field marks an extended XSAVE area as present.
const FP_SW_RESERVED: usize = 464;
/// `magic1` value the kernel stamps when a signal frame carries an XSAVE extended area.
const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
/// Byte offset of the XSAVE header (`xstate_bv` first) after the 512-byte legacy area.
const XSAVE_HEADER: usize = 512;
/// XSTATE_BV bit for the AVX YMM_Hi128 component (bits 255:128 of each YMM register).
const XFEATURE_YMM: u64 = 1 << 2;

/// Final architectural state, written by the child's signal handler into the shared
/// page and read back by the parent. `#[repr(C)]` so both sides agree on layout.
#[repr(C)]
#[derive(Clone, Copy)]
struct Capture {
    /// 0 = nothing captured, 1 = clean `hlt` terminator, 2 = other fault (skip).
    status: u64,
    rip: u64,
    rflags: u64,
    fault_addr: u64,
    gpr: [u64; 16],
    xmm: [u128; 16],
    /// Bits 255:128 of each YMM register, read from the signal XSAVE area (task-191).
    /// Left zero when the host lacks AVX or the frame's YMM component is init-optimized
    /// (all-zero) — both of which correctly mean "upper halves are zero".
    ymm_hi: [u128; 16],
}

const CAP_NONE: u64 = 0;
const CAP_HLT: u64 = 1;
const CAP_FAULT: u64 = 2;

/// A dedicated stack for the fault handler: the guest runs with its own `RSP` (0 in
/// the fuzzer), so the kernel can't push the signal frame onto the guest stack.
const ALTSTACK_LEN: usize = 64 * 1024;
static mut ALTSTACK: [u8; ALTSTACK_LEN] = [0u8; ALTSTACK_LEN];

/// x86 GPR-index (RAX,RCX,RDX,RBX,RSP,RBP,RSI,RDI,R8..R15) → Linux `gregs[]` index.
const GREG_OF: [usize; 16] = [
    libc::REG_RAX as usize,
    libc::REG_RCX as usize,
    libc::REG_RDX as usize,
    libc::REG_RBX as usize,
    libc::REG_RSP as usize,
    libc::REG_RBP as usize,
    libc::REG_RSI as usize,
    libc::REG_RDI as usize,
    libc::REG_R8 as usize,
    libc::REG_R9 as usize,
    libc::REG_R10 as usize,
    libc::REG_R11 as usize,
    libc::REG_R12 as usize,
    libc::REG_R13 as usize,
    libc::REG_R14 as usize,
    libc::REG_R15 as usize,
];

/// The fault handler (runs in the child, on the altstack). Async-signal-safe: it only
/// touches raw memory and calls `_exit`. On the guest's terminating `hlt` it snapshots
/// the register file; any other signal marks the run unusable so the parent skips it.
///
/// A userspace `hlt` is a general-protection fault, which the kernel reports as a
/// `SIGSEGV` with `si_code == SI_KERNEL` and no meaningful faulting address. A paging
/// fault (exec on an NX page, unmapped RIP) instead carries `SEGV_ACCERR`/`SEGV_MAPERR`
/// with `si_addr == RIP`. Gating on `SI_KERNEL` before dereferencing RIP is what keeps
/// this sound: we only read `*(rip)` — to confirm the `0xf4` opcode — once we know the
/// fault is a `#GP`, never on a page fault whose RIP may be unmapped or point at a data
/// byte that happens to be `0xf4` on a mapped-but-non-executable control page.
extern "C" fn handler(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: `ctx` is a `*mut ucontext_t` and `info` a `*mut siginfo_t` (SA_SIGINFO);
    // SHARE is a mapped RW page. RIP is dereferenced only under the SI_KERNEL gate
    // below, i.e. only for a `#GP` where RIP is the executing (mapped) instruction.
    unsafe {
        let uc = &*(ctx as *const libc::ucontext_t);
        let gregs = &uc.uc_mcontext.gregs;
        let rip = gregs[libc::REG_RIP as usize] as u64;
        let si_code = (*info).si_code;
        let cap = &mut *(SHARE as *mut Capture);

        let is_hlt =
            sig == libc::SIGSEGV && si_code == libc::SI_KERNEL && *(rip as *const u8) == 0xf4;
        if !is_hlt {
            cap.status = CAP_FAULT;
            cap.fault_addr = rip;
            libc::_exit(0);
        }

        for (i, &g) in GREG_OF.iter().enumerate() {
            cap.gpr[i] = gregs[g] as u64;
        }
        // Engine convention: RIP resumes *past* the `hlt` (1 byte), matching Unicorn.
        cap.rip = rip + 1;
        cap.rflags = gregs[libc::REG_EFL as usize] as u64;

        let fp = uc.uc_mcontext.fpregs;
        if !fp.is_null() {
            for (i, slot) in cap.xmm.iter_mut().enumerate() {
                let e = (*fp)._xmm[i].element;
                *slot = (e[0] as u128)
                    | ((e[1] as u128) << 32)
                    | ((e[2] as u128) << 64)
                    | ((e[3] as u128) << 96);
            }
            // YMM upper halves (task-191): the extended XSAVE area follows the 512-byte
            // legacy FXSAVE region. It's present only when `_fpx_sw_bytes.magic1` is set;
            // the YMM component sits at the host's XSAVE offset (passed in via the control
            // page, 0 when the host has no AVX). A cleared `XFEATURE_YMM` bit in the frame
            // means the upper halves are all-zero (init optimization) — leave them 0.
            let ymm_off = core::ptr::read_unaligned((CTRL + IN_YMM_OFFSET) as *const u32) as usize;
            if ymm_off != 0 {
                let base = fp as *const u8;
                let magic = core::ptr::read_unaligned(base.add(FP_SW_RESERVED) as *const u32);
                let xstate_bv = core::ptr::read_unaligned(base.add(XSAVE_HEADER) as *const u64);
                if magic == FP_XSTATE_MAGIC1 && xstate_bv & XFEATURE_YMM != 0 {
                    for (i, slot) in cap.ymm_hi.iter_mut().enumerate() {
                        *slot =
                            core::ptr::read_unaligned(base.add(ymm_off + i * 16) as *const u128);
                    }
                }
            }
        }
        cap.status = CAP_HLT;
        libc::_exit(0);
    }
}

/// `mmap` `len` bytes at exactly `addr` (page-aligned). `NOREPLACE` so we never
/// clobber an existing host mapping — a collision just means "native unavailable".
fn map_fixed(addr: u64, len: usize, prot: libc::c_int, shared: bool) -> bool {
    let flags = libc::MAP_ANONYMOUS
        | libc::MAP_FIXED_NOREPLACE
        | if shared {
            libc::MAP_SHARED
        } else {
            libc::MAP_PRIVATE
        };
    // SAFETY: anonymous mapping at a fixed address; result checked below.
    let p = unsafe { libc::mmap(addr as *mut libc::c_void, len, prot, flags, -1, 0) };
    p as u64 == addr
}

fn unmap(addr: u64, len: usize) {
    // SAFETY: unmaps a region this module mapped.
    unsafe {
        libc::munmap(addr as *mut libc::c_void, len);
    }
}

/// Owns the fixed regions `run_native` maps and unmaps every one on `Drop`, so an
/// early `return None` (or a panic mid-run) can't leak the control/guest window and
/// wedge the next run's `MAP_FIXED_NOREPLACE`.
struct Mappings(Vec<(u64, usize)>);

impl Drop for Mappings {
    fn drop(&mut self) {
        for &(addr, len) in &self.0 {
            unmap(addr, len);
        }
    }
}

/// Page-aligned `[start, end)` byte span covering a chunk.
fn chunk_span(c: &MemChunk) -> (u64, usize) {
    let start = c.addr & !(PAGE - 1);
    let end = (c.addr + c.bytes.len() as u64 + PAGE - 1) & !(PAGE - 1);
    (start, (end - start) as usize)
}

/// Byte offset of the AVX `YMM_Hi128` component in the host's non-compacted XSAVE area
/// (CPUID leaf 0xD, sub-leaf 2), cached. `0` means the host has no AVX, in which case
/// there are no YMM upper halves to capture and the stub skips `vzeroall`.
fn host_ymm_offset() -> u32 {
    use std::sync::OnceLock;
    static OFF: OnceLock<u32> = OnceLock::new();
    *OFF.get_or_init(|| {
        if std::is_x86_feature_detected!("avx") {
            // Leaf 0xD sub-leaf 2 is defined whenever AVX/XSAVE is present; EBX is the
            // YMM component's byte offset in the standard XSAVE layout the kernel uses.
            std::arch::x86_64::__cpuid_count(0xD, 2).ebx
        } else {
            0
        }
    })
}

/// Assemble the register-load stub: (on an AVX host) `vzeroall` to clear the YMM upper
/// halves so an untouched register reads back zero — matching the interpreter's
/// zero-initialized `ymm_hi`, not the child's inherited-dirty FPU state — then load XMM,
/// flags, and all GPRs from the input block, and `jmp` (indirect, through the input
/// block) to the guest entry. Loading flags via `push`/`popfq` happens *before* `RSP`
/// is overwritten; the final GPR is RAX and the jump reads its target from memory, so no
/// register is clobbered late.
fn assemble_stub(avx: bool) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    if avx {
        // Zero YMM0-15 (full width) before the XMM loads below re-establish the low 128.
        a.vzeroall().unwrap();
    }
    let xmms = [
        xmm0, xmm1, xmm2, xmm3, xmm4, xmm5, xmm6, xmm7, xmm8, xmm9, xmm10, xmm11, xmm12, xmm13,
        xmm14, xmm15,
    ];
    for (i, x) in xmms.into_iter().enumerate() {
        a.movdqu(x, xmmword_ptr(CTRL + IN_XMM + (i * 16) as u64))
            .unwrap();
    }
    // flags: mov rax,[rflags]; push rax; popfq  (uses the host stack, still valid)
    a.mov(rax, qword_ptr(CTRL + IN_RFLAGS)).unwrap();
    a.push(rax).unwrap();
    a.popfq().unwrap();
    // GPRs — RSP anywhere after popfq; RAX last so the base isn't needed afterward.
    let g = |idx: u64| qword_ptr(CTRL + IN_GPR + idx * 8);
    a.mov(rbx, g(3)).unwrap();
    a.mov(rcx, g(1)).unwrap();
    a.mov(rdx, g(2)).unwrap();
    a.mov(rbp, g(5)).unwrap();
    a.mov(rsi, g(6)).unwrap();
    a.mov(rdi, g(7)).unwrap();
    a.mov(r8, g(8)).unwrap();
    a.mov(r9, g(9)).unwrap();
    a.mov(r10, g(10)).unwrap();
    a.mov(r11, g(11)).unwrap();
    a.mov(r12, g(12)).unwrap();
    a.mov(r13, g(13)).unwrap();
    a.mov(r14, g(14)).unwrap();
    a.mov(r15, g(15)).unwrap();
    a.mov(rsp, g(4)).unwrap();
    a.mov(rax, g(0)).unwrap();
    a.jmp(qword_ptr(CTRL + IN_ENTRY)).unwrap();
    a.assemble(STUB).unwrap()
}

/// Run `input` on the real host CPU. Returns `None` when the snippet can't be run
/// natively — so the caller skips it (the interpreter/Unicorn still cover those).
/// `None` is returned for: a `RunSpec` other than `UntilExit` (native runs only to the
/// terminating `hlt`); a nonzero FS/GS base (we can't set guest segment bases from user
/// mode); a snippet containing the `syscall` opcode (a live host syscall with guest
/// registers is unsafe to execute); a guest VA below `mmap_min_addr`; a control-page
/// collision; an unsupported instruction (`SIGILL`); a non-`hlt` fault; or a timeout.
///
/// Serialized process-wide: the fixed control/guest VAs can host only one run at a
/// time. The whole body holds a mutex.
pub fn run_native(input: &VectorInput) -> Option<RunOutcome> {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Native runs the snippet to its terminating `hlt`; a block-count spec has no
    // meaning here.
    if input.run != RunSpec::UntilExit {
        return None;
    }
    // We don't program guest FS/GS bases (arch_prctl in the child would touch the
    // child's own TLS, not model a guest base), so a nonzero-base input can't be run
    // faithfully — skip it rather than lie.
    if input.cpu_init.fs_base != 0 || input.cpu_init.gs_base != 0 {
        return None;
    }
    // The stub loads only the low 128 bits of each vector register (XMM) and `vzeroall`s
    // the upper halves; it can't establish a nonzero YMM/ZMM init, so reject such an
    // input rather than run it with the wrong upper halves.
    if input.cpu_init.ymm_hi.iter().any(|&v| v != 0) {
        return None;
    }
    // A guest `syscall` (0f 05) would issue a *real* host syscall with guest-controlled
    // registers in the child — refuse to run any snippet whose code contains that
    // 2-byte sequence. Scanning raw bytes is conservative: a false positive (the pair
    // appearing as data) only skips the input, which is the safe direction.
    for c in &input.mem_init {
        if c.bytes.windows(2).any(|w| w == [0x0f, 0x05]) {
            return None;
        }
    }

    // Guest VAs must clear mmap_min_addr and not collide with the control window.
    for c in &input.mem_init {
        let (start, len) = chunk_span(c);
        if start < 0x1_0000 || overlaps(start, len, CTRL, (STUB + PAGE - CTRL) as usize) {
            return None;
        }
    }

    // Map control window + guest memory. Guest pages are SHARED so the child's memory
    // writes are visible for read-back; the input/stub pages are private (read-only
    // to the child); the capture page is shared (the handler writes it). The `Mappings`
    // guard unmaps everything on every exit path, panic included.
    let mut guard = Mappings(Vec::new());
    let rw = libc::PROT_READ | libc::PROT_WRITE;
    let rwx = rw | libc::PROT_EXEC;

    if !map_fixed(CTRL, PAGE as usize, rw, false) {
        return None;
    }
    guard.0.push((CTRL, PAGE as usize));
    if !map_fixed(SHARE, PAGE as usize, rw, true) {
        return None;
    }
    guard.0.push((SHARE, PAGE as usize));
    if !map_fixed(STUB, PAGE as usize, rwx, false) {
        return None;
    }
    guard.0.push((STUB, PAGE as usize));

    // Guest chunks (dedup pages: chunks may share one, and double-mapping fails).
    for c in &input.mem_init {
        let (start, len) = chunk_span(c);
        if guard.0.iter().any(|&(a, l)| a == start && l == len) {
            continue;
        }
        if !map_fixed(start, len, rwx, true) {
            return None;
        }
        guard.0.push((start, len));
    }

    // Fill guest bytes and the input block.
    // SAFETY: every address written below lives in a page just mapped RW/RWX.
    unsafe {
        for c in &input.mem_init {
            std::ptr::copy_nonoverlapping(c.bytes.as_ptr(), c.addr as *mut u8, c.bytes.len());
        }
        let init = &input.cpu_init;
        let gpr = (CTRL + IN_GPR) as *mut u64;
        for (i, &v) in init.gpr.iter().enumerate() {
            // RSP/RBP come from the snapshot; the fuzzer leaves them 0 (unused).
            gpr.add(i).write(v);
        }
        // We don't program guest FS/GS bases (nonzero-base inputs are rejected above),
        // so the child keeps the parent's inherited glibc TLS FS base — harmless as
        // long as the guest never dereferences an FS/GS-relative address.
        ((CTRL + IN_RFLAGS) as *mut u64).write(init.flags.to_rflags());
        ((CTRL + IN_ENTRY) as *mut u64).write(input.entry);
        let xmm = (CTRL + IN_XMM) as *mut u128;
        for (i, &v) in init.xmm.iter().enumerate() {
            xmm.add(i).write(v);
        }
        // Where the handler finds the YMM upper halves in the signal XSAVE area (0 = no
        // AVX host → skip YMM capture, and the stub skips `vzeroall`).
        let ymm_off = host_ymm_offset();
        ((CTRL + IN_YMM_OFFSET) as *mut u32).write(ymm_off);
        let stub = assemble_stub(ymm_off != 0);
        std::ptr::copy_nonoverlapping(stub.as_ptr(), STUB as *mut u8, stub.len());

        (*(SHARE as *mut Capture)).status = CAP_NONE;
    }

    // Fork and run the guest in the child.
    // SAFETY: fork; the child path is async-signal-safe until it enters the guest.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return None;
    }
    if pid == 0 {
        // Child: install the fault handler on its own altstack, cap runtime, and jump
        // into the stub. Never returns. Arming the handler here (not in the parent)
        // keeps the parent's own fatal-signal reporters intact.
        // SAFETY: only async-signal-safe calls (`sigaction`/`sigaltstack`/`alarm`) run
        // between `fork` and the jump into guest code; ALTSTACK is written solely by
        // this child (the parent never touches it), so the static is exclusively ours.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut sa.sa_mask);
            for sig in [
                libc::SIGSEGV,
                libc::SIGILL,
                libc::SIGBUS,
                libc::SIGTRAP,
                libc::SIGFPE,
                libc::SIGALRM,
            ] {
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
            let alt = libc::stack_t {
                ss_sp: core::ptr::addr_of_mut!(ALTSTACK) as *mut libc::c_void,
                ss_flags: 0,
                ss_size: ALTSTACK_LEN,
            };
            libc::sigaltstack(&alt, std::ptr::null_mut());
            libc::alarm(2); // runaway guard (SIGALRM → CAP_FAULT → skip)
            core::arch::asm!("jmp {0}", in(reg) STUB, options(noreturn));
        }
    }

    // Parent: wait for the child, then read the capture + guest memory back.
    // SAFETY: waitpid on our child; SHARE is a mapped shared page.
    let mut status = 0;
    unsafe {
        libc::waitpid(pid, &mut status, 0);
    }
    // SAFETY: SHARE stays mapped until `guard` drops at end of scope; the child has
    // exited (waitpid returned) so there is no concurrent writer to the capture page.
    let cap = unsafe { *(SHARE as *const Capture) };

    let outcome = if cap.status == CAP_HLT {
        Some(RunOutcome {
            cpu: CpuSnapshot {
                gpr: cap.gpr,
                rip: cap.rip,
                flags: SnapFlags::from_rflags(cap.rflags),
                // Both guaranteed 0: nonzero-base inputs are rejected above.
                fs_base: input.cpu_init.fs_base,
                gs_base: input.cpu_init.gs_base,
                xmm: cap.xmm,
                // Captured from the signal XSAVE area on an AVX host (task-191); zero on
                // a non-AVX host or when the frame's YMM component is init-optimized.
                ymm_hi: cap.ymm_hi,
            },
            mem: read_back(&input.mem_init),
            exit: ExitKind::Hlt,
        })
    } else {
        None
    };

    // `guard` unmaps every region as it drops here.
    outcome
}

/// Read each `mem_init` region back from its (still-mapped) guest VA.
fn read_back(chunks: &[MemChunk]) -> Vec<MemChunk> {
    chunks
        .iter()
        .map(|c| {
            let mut bytes = vec![0u8; c.bytes.len()];
            // SAFETY: the region is mapped until `cleanup` runs after this.
            unsafe {
                std::ptr::copy_nonoverlapping(c.addr as *const u8, bytes.as_mut_ptr(), bytes.len());
            }
            MemChunk {
                addr: c.addr,
                bytes,
                kind: c.kind,
            }
        })
        .collect()
}

fn overlaps(a: u64, alen: usize, b: u64, blen: usize) -> bool {
    a < b + blen as u64 && b < a + alen as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{CpuSnapshot, MemKind, RunSpec};

    /// Pin the oracle mechanism end-to-end, independent of the fuzzer/interpreter:
    /// assemble a snippet that computes GPR, XMM and memory results, run it on the
    /// real CPU, and assert the captured state is exactly what the code produced.
    #[test]
    fn native_captures_real_cpu_state() {
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;

        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(eax, 0x1234i32).unwrap();
        a.add(eax, 1).unwrap(); // rax = 0x1235
        a.mov(ebx, 0xFFi32).unwrap();
        a.popcnt(ecx, ebx).unwrap(); // rcx = 8 (a VEX-free BMI-adjacent op)
        a.mov(qword_ptr(scratch), rax).unwrap(); // memory write-back
        a.movd(xmm3, eax).unwrap(); // xmm3 low dword = 0x1235
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let input = VectorInput {
            cpu_init: CpuSnapshot::default(),
            mem_init: vec![
                MemChunk {
                    addr: code,
                    bytes,
                    kind: MemKind::Ram,
                },
                MemChunk {
                    addr: scratch,
                    bytes: vec![0u8; 0x1000],
                    kind: MemKind::Ram,
                },
            ],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let out = run_native(&input).expect("host runs a trivial snippet natively");
        assert_eq!(out.cpu.gpr[0], 0x1235, "rax");
        assert_eq!(out.cpu.gpr[1], 8, "rcx = popcnt(0xff)");
        assert_eq!(out.cpu.xmm[3] & 0xFFFF_FFFF, 0x1235, "xmm3 low dword");
        assert_eq!(out.exit, ExitKind::Hlt);
        let s = out.mem.iter().find(|c| c.addr == scratch).unwrap();
        assert_eq!(&s.bytes[..8], &0x1235u64.to_le_bytes(), "memory write-back");
    }

    /// task-191: an AVX snippet that writes a YMM register's upper half is oracled
    /// native-vs-interp, and the captured `ymm_hi` is exactly the value the code loaded —
    /// proving the XSAVE-area YMM capture, not a trivially-zero upper half.
    #[test]
    fn native_captures_ymm_upper_half() {
        // Skip on a host without AVX (no YMM to capture; `vmovdqu ymm` would #UD → skip).
        if host_ymm_offset() == 0 {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;

        // 32-byte source with distinct low/high 128-bit halves, so the assertion on the
        // upper half can't pass by coincidence with the lower.
        let pattern: Vec<u8> = (0..32u8).collect();
        let lo = u128::from_le_bytes(pattern[..16].try_into().unwrap());
        let hi = u128::from_le_bytes(pattern[16..].try_into().unwrap());
        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..32].copy_from_slice(&pattern);

        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu(ymm2, ymmword_ptr(scratch)).unwrap(); // ymm2 = 256-bit pattern
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let input = VectorInput {
            cpu_init: CpuSnapshot::default(),
            mem_init: vec![
                MemChunk {
                    addr: code,
                    bytes,
                    kind: MemKind::Ram,
                },
                MemChunk {
                    addr: scratch,
                    bytes: scratch_page,
                    kind: MemKind::Ram,
                },
            ],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX host runs a vmovdqu-ymm snippet");
        assert_eq!(native.cpu.xmm[2], lo, "ymm2 low 128 bits");
        assert_eq!(
            native.cpu.ymm_hi[2], hi,
            "ymm2 upper 128 bits (the XSAVE capture)"
        );
        assert_ne!(hi, 0, "the test's upper half must be non-trivial");

        // And the real CPU agrees with the interpreter on the full state (the oracle).
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "native diverges from interpreter on a YMM write:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }
}
