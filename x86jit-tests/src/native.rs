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
//! Captures GPRs, RIP, RFLAGS, the 16 XMM registers, and — read from the extended XSAVE
//! area of the signal frame — the YMM upper halves (AVX host, task-191) plus the ZMM
//! upper halves (bits 511:256) and opmask `k` registers (AVX-512 host, task-193). The
//! stub first clears the corresponding registers (`vzeroall`, or `vpxorq zmm`/`kxorq` on
//! AVX-512) so an untouched register/mask reads back zero — matching the interpreter's
//! zero-init, not the child's inherited-dirty state. Registers 0–15 only (the snapshot
//! width); `zmm16-31` are not captured.

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
const IN_YMM_OFFSET: u64 = 400; // u32: XSAVE byte offset of the YMM component (0 = no AVX)
const IN_K_OFFSET: u64 = 404; //   u32: XSAVE byte offset of the opmask component (task-193)
const IN_ZMM_OFFSET: u64 = 408; // u32: XSAVE byte offset of ZMM_Hi256 (0 = no AVX-512)
const IN_YMM_HI: u64 = 416; //   [u128; 16], bits 255:128 of ymm0-15 (loaded via vinsertf128)

/// Byte offset of the `_fpx_sw_bytes` block inside the 512-byte legacy FXSAVE area of a
/// signal `fpstate` — its `magic1` field marks an extended XSAVE area as present.
const FP_SW_RESERVED: usize = 464;
/// `magic1` value the kernel stamps when a signal frame carries an XSAVE extended area.
const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
/// Byte offset of the XSAVE header (`xstate_bv` first) after the 512-byte legacy area.
const XSAVE_HEADER: usize = 512;
/// XSTATE_BV bit for the AVX YMM_Hi128 component (bits 255:128 of each YMM register).
const XFEATURE_YMM: u64 = 1 << 2;
/// XSTATE_BV bit for the AVX-512 opmask (k0–k7) component (task-193).
const XFEATURE_OPMASK: u64 = 1 << 5;
/// XSTATE_BV bit for the AVX-512 ZMM_Hi256 component (bits 511:256 of zmm0–15).
const XFEATURE_ZMM_HI256: u64 = 1 << 6;

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
    /// Bits 511:256 of each ZMM register (task-193): `[bits 383:256, bits 511:384]`.
    zmm_hi: [[u128; 2]; 16],
    /// Opmask registers k0–k7 (task-193).
    kmask: [u64; 8],
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
                if magic == FP_XSTATE_MAGIC1 {
                    if xstate_bv & XFEATURE_YMM != 0 {
                        for (i, slot) in cap.ymm_hi.iter_mut().enumerate() {
                            *slot = core::ptr::read_unaligned(
                                base.add(ymm_off + i * 16) as *const u128
                            );
                        }
                    }
                    // Opmask (k0–k7) and ZMM upper halves (task-193): each component sits
                    // at its host XSAVE offset; a cleared XSTATE_BV bit means all-zero.
                    let k_off =
                        core::ptr::read_unaligned((CTRL + IN_K_OFFSET) as *const u32) as usize;
                    if k_off != 0 && xstate_bv & XFEATURE_OPMASK != 0 {
                        for (i, slot) in cap.kmask.iter_mut().enumerate() {
                            *slot =
                                core::ptr::read_unaligned(base.add(k_off + i * 8) as *const u64);
                        }
                    }
                    let zmm_off =
                        core::ptr::read_unaligned((CTRL + IN_ZMM_OFFSET) as *const u32) as usize;
                    if zmm_off != 0 && xstate_bv & XFEATURE_ZMM_HI256 != 0 {
                        for (i, slot) in cap.zmm_hi.iter_mut().enumerate() {
                            // 32 bytes per register: [bits 383:256, bits 511:384].
                            slot[0] = core::ptr::read_unaligned(
                                base.add(zmm_off + i * 32) as *const u128
                            );
                            slot[1] = core::ptr::read_unaligned(
                                base.add(zmm_off + i * 32 + 16) as *const u128
                            );
                        }
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

/// Host XSAVE component byte offsets `(ymm, opmask, zmm_hi256)` in the standard layout
/// (CPUID leaf 0xD sub-leaves 2/5/6), cached. A `0` offset means that component is
/// absent: `ymm == 0` ⇒ no AVX (stub skips `vzeroall`, no YMM capture); `zmm == 0` ⇒
/// no AVX-512 (stub skips the EVEX zeroing, no ZMM/opmask capture).
fn host_xsave_offsets() -> (u32, u32, u32) {
    use std::sync::OnceLock;
    static OFF: OnceLock<(u32, u32, u32)> = OnceLock::new();
    *OFF.get_or_init(|| {
        let ymm = if std::is_x86_feature_detected!("avx") {
            std::arch::x86_64::__cpuid_count(0xD, 2).ebx
        } else {
            0
        };
        let (k, zmm) = if std::is_x86_feature_detected!("avx512f") {
            (
                std::arch::x86_64::__cpuid_count(0xD, 5).ebx,
                std::arch::x86_64::__cpuid_count(0xD, 6).ebx,
            )
        } else {
            (0, 0)
        };
        (ymm, k, zmm)
    })
}

/// Assemble the register-load stub: (on an AVX host) `vzeroall` to clear the YMM upper
/// halves so an untouched register reads back zero — matching the interpreter's
/// zero-initialized `ymm_hi`, not the child's inherited-dirty FPU state — then load XMM,
/// flags, and all GPRs from the input block, and `jmp` (indirect, through the input
/// block) to the guest entry. Loading flags via `push`/`popfq` happens *before* `RSP`
/// is overwritten; the final GPR is RAX and the jump reads its target from memory, so no
/// register is clobbered late.
fn assemble_stub(avx: bool, avx512: bool) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    if avx512 {
        // Zero the full ZMM0-15 (bits 511:0) and all opmasks so an untouched register or
        // mask reads back zero, matching the interpreter's zero-init (task-193). `vpxorq`
        // zeroes the whole 512-bit register; the XMM loads below re-establish bits 127:0.
        let zmms = [
            zmm0, zmm1, zmm2, zmm3, zmm4, zmm5, zmm6, zmm7, zmm8, zmm9, zmm10, zmm11, zmm12, zmm13,
            zmm14, zmm15,
        ];
        for z in zmms {
            a.vpxorq(z, z, z).unwrap();
        }
        let ks = [k0, k1, k2, k3, k4, k5, k6, k7];
        for kk in ks {
            a.kxorq(kk, kk, kk).unwrap();
        }
    } else if avx {
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
    if avx {
        // Load bits 255:128 of each YMM from the input block. `vinsertf128 .. ,1`
        // replaces the upper half only, leaving the low 128 set by the movdqu above.
        // Lets the native replay establish a full 256-bit AVX2 pre-state (task-215
        // lockstep tracer), not just the low lane.
        let ymms = [
            ymm0, ymm1, ymm2, ymm3, ymm4, ymm5, ymm6, ymm7, ymm8, ymm9, ymm10, ymm11, ymm12, ymm13,
            ymm14, ymm15,
        ];
        for (i, y) in ymms.into_iter().enumerate() {
            a.vinsertf128(y, y, xmmword_ptr(CTRL + IN_YMM_HI + (i * 16) as u64), 1)
                .unwrap();
        }
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
    // The stub loads XMM (low 128) plus — on an AVX host — the YMM upper halves via
    // vinsertf128. It cannot establish a nonzero ZMM_Hi256 or opmask init, so reject
    // those; reject a nonzero YMM upper only when the host lacks AVX to load it.
    if input.cpu_init.zmm_hi.iter().flatten().any(|&v| v != 0)
        || input.cpu_init.kmask.iter().any(|&v| v != 0)
        || (input.cpu_init.ymm_hi.iter().any(|&v| v != 0) && !std::is_x86_feature_detected!("avx"))
    {
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
        // YMM upper halves; the stub loads these via vinsertf128 on an AVX host.
        let ymm_hi = (CTRL + IN_YMM_HI) as *mut u128;
        for (i, &v) in init.ymm_hi.iter().enumerate() {
            ymm_hi.add(i).write(v);
        }
        // Where the handler finds each XSAVE component (0 = absent → skip that capture,
        // and the stub skips the corresponding zeroing).
        let (ymm_off, k_off, zmm_off) = host_xsave_offsets();
        ((CTRL + IN_YMM_OFFSET) as *mut u32).write(ymm_off);
        ((CTRL + IN_K_OFFSET) as *mut u32).write(k_off);
        ((CTRL + IN_ZMM_OFFSET) as *mut u32).write(zmm_off);
        let stub = assemble_stub(ymm_off != 0, zmm_off != 0);
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
                // ZMM upper halves + opmasks captured on an AVX-512 host (task-193).
                zmm_hi: cap.zmm_hi,
                kmask: cap.kmask,
                // The native oracle does not capture x87 state (task-188); leave the
                // stack at the snapshot default. Native x87 differential is out of scope.
                ..Default::default()
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

    /// task-215 lockstep tracer — replay side. Reads a trace file produced by the
    /// interpreter's `X86JIT_LOCKSTEP` capture (each record = one register-only vector
    /// instruction with its pre/post ymm0-15 state as computed by our interpreter),
    /// re-runs each op on the real host CPU from the same pre-state, and reports the
    /// first op whose native result diverges from the captured (interpreter) post-state
    /// — i.e. the exact op, with openssl's real operands, that we compute wrong.
    ///
    /// Gated on `X86JIT_LOCKSTEP_REPLAY=<trace-path>`; a normal test run skips it. Run:
    ///   X86JIT_LOCKSTEP_REPLAY=/tmp/trace.bin \
    ///     cargo test -p x86jit-tests replay_lockstep_trace -- --nocapture --ignored
    #[test]
    #[ignore = "forensic tool; needs X86JIT_LOCKSTEP_REPLAY=<trace> from an interp run"]
    fn replay_lockstep_trace() {
        use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, NasmFormatter};
        use std::collections::HashSet;

        let Some(path) = std::env::var_os("X86JIT_LOCKSTEP_REPLAY") else {
            eprintln!("X86JIT_LOCKSTEP_REPLAY unset — nothing to replay");
            return;
        };
        // mmap the (multi-GB) trace read-only so parallel shard processes share the
        // page cache instead of each copying it into RSS.
        use std::os::unix::io::AsRawFd;
        let file = std::fs::File::open(&path).expect("open trace file");
        let len = file.metadata().expect("stat trace").len() as usize;
        let data: &[u8] = if len == 0 {
            &[]
        } else {
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_PRIVATE,
                    file.as_raw_fd(),
                    0,
                )
            };
            assert!(p != libc::MAP_FAILED, "mmap trace file");
            unsafe { std::slice::from_raw_parts(p as *const u8, len) }
        };
        if !std::is_x86_feature_detected!("avx2") {
            eprintln!("host lacks AVX2 — cannot replay the trace natively");
            return;
        }

        // v3 side-state wire layout (must match x86jit-core/src/lockstep.rs):
        //   gpr[16]=128 | flags=8 | mem[64] | xmm[16]=256 | ymm_hi[16]=256  = 712 bytes
        const GPRB: usize = 16 * 8;
        const MEMB: usize = 64;
        const SNAP: usize = 32 * 16; // full vec snapshot: 16 xmm + 16 ymm_hi
        const SIDE: usize = GPRB + 8 + MEMB + SNAP;
        // Arithmetic flags that drive branches: CF|PF|ZF|SF|OF (AF excluded — some vector
        // ops leave it model-defined and it never gates a branch here).
        const FLAG_MASK: u64 = 0x8C5;
        struct Side {
            gpr: [u64; 16],
            flags: u64,
            mem: [u8; MEMB],
            xmm: [u128; 16],
            ymm: [u128; 16],
        }
        let read_side = |b: &[u8]| -> Side {
            let mut gpr = [0u64; 16];
            for (i, g) in gpr.iter_mut().enumerate() {
                *g = u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
            }
            let flags = u64::from_le_bytes(b[GPRB..GPRB + 8].try_into().unwrap());
            let mut mem = [0u8; MEMB];
            mem.copy_from_slice(&b[GPRB + 8..GPRB + 8 + MEMB]);
            let s = &b[GPRB + 8 + MEMB..];
            let mut xmm = [0u128; 16];
            let mut ymm = [0u128; 16];
            for i in 0..16 {
                xmm[i] = u128::from_le_bytes(s[i * 16..i * 16 + 16].try_into().unwrap());
                ymm[i] =
                    u128::from_le_bytes(s[256 + i * 16..256 + i * 16 + 16].try_into().unwrap());
            }
            Side {
                gpr,
                flags,
                mem,
                xmm,
                ymm,
            }
        };
        let disasm = |bytes: &[u8], ip: u64| -> String {
            let mut dec = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
            let mut insn = Instruction::default();
            dec.decode_out(&mut insn);
            let mut s = String::new();
            NasmFormatter::new().format(&insn, &mut s);
            s
        };

        // Optional sharding: N parallel processes each own the fixed native VAs in their
        // own address space, so we split the record stream `total % shards == shard`.
        // `total` (the global scan index) is printed at divergence, so the earliest bug
        // across shards is the one with the smallest `total`.
        let shards: u64 = std::env::var("X86JIT_LOCKSTEP_SHARDS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let shard: u64 = std::env::var("X86JIT_LOCKSTEP_SHARD")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let cmp_flags = std::env::var_os("X86JIT_LOCKSTEP_FLAGS").is_some();

        let mut off = 0usize;
        let (mut total, mut replayed, mut skipped) = (0u64, 0u64, 0u64);
        let mut seen: HashSet<u64> = HashSet::new();
        while off + 18 <= data.len() {
            // Layout: addr | blen | bytes | has_mem | ea | pre-side(712) | post-side(712)
            let addr = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            let blen = data[off + 8] as usize;
            let hd = off + 9 + blen;
            let has_mem = data[hd] != 0;
            let ea = u64::from_le_bytes(data[hd + 1..hd + 9].try_into().unwrap());
            let pre0 = hd + 9;
            let rec_end = pre0 + 2 * SIDE;
            if rec_end > data.len() {
                break; // truncated trailing record (capture cut off mid-write) — ignore
            }
            let bytes = &data[off + 9..hd];
            let pre = read_side(&data[pre0..pre0 + SIDE]);
            let post = read_side(&data[pre0 + SIDE..rec_end]);
            let rec_start = off;
            off = rec_end;
            total += 1;
            if shards > 1 && (total - 1) % shards != shard {
                continue;
            }

            // Skip exact-duplicate records (loops replay the same op millions of times);
            // a divergence on given operands shows up on its first occurrence.
            let mut h = 0xcbf29ce484222325u64;
            for &byte in &data[rec_start..rec_end] {
                h = (h ^ byte as u64).wrapping_mul(0x100000001b3);
            }
            if !seen.insert(h) {
                continue;
            }

            let mut code = bytes.to_vec();
            code.push(0xf4); // hlt terminator
                             // Code runs at its ORIGINAL guest address so any RIP-relative memory operand
                             // resolves to the same EA we captured and mapped below.
            let mut mem_init = vec![MemChunk {
                addr,
                bytes: code,
                kind: MemKind::Ram,
            }];
            if has_mem {
                mem_init.push(MemChunk {
                    addr: ea,
                    bytes: pre.mem.to_vec(),
                    kind: MemKind::Ram,
                });
            }
            let input = VectorInput {
                cpu_init: CpuSnapshot {
                    gpr: pre.gpr,
                    flags: SnapFlags::from_rflags(pre.flags),
                    xmm: pre.xmm,
                    ymm_hi: pre.ymm,
                    ..Default::default()
                },
                mem_init,
                entry: addr,
                run: RunSpec::UntilExit,
            };
            let Some(out) = run_native(&input) else {
                skipped += 1;
                continue;
            };
            replayed += 1;
            if replayed % 500 == 0 {
                eprintln!(
                    "  ..{replayed} replayed, {} unique, {total} scanned (at {addr:#x})",
                    seen.len()
                );
            }

            let report = |what: String| -> ! {
                panic!(
                    "DIVERGENCE at guest {addr:#x}: {}\n  bytes: {}\n  {what}\n  \
                     (after {replayed} replayed, {skipped} skipped, {total} scanned)",
                    disasm(bytes, addr),
                    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                );
            };
            for r in 0..16 {
                if out.cpu.xmm[r] != post.xmm[r] || out.cpu.ymm_hi[r] != post.ymm[r] {
                    report(format!(
                        "ymm{r}: interp {:032x}:{:032x} vs native {:032x}:{:032x}",
                        post.ymm[r], post.xmm[r], out.cpu.ymm_hi[r], out.cpu.xmm[r]
                    ));
                }
            }
            for r in 0..16 {
                if out.cpu.gpr[r] != post.gpr[r] {
                    report(format!(
                        "gpr[{r}]: interp {:#018x} vs native {:#018x}",
                        post.gpr[r], out.cpu.gpr[r]
                    ));
                }
            }
            // Flag comparison is opt-in (X86JIT_LOCKSTEP_FLAGS=1) and DELIBERATELY not
            // part of the default pass, because the interpreter elides dead flags: when
            // an op's flags are overwritten before any read, the lifter emits
            // FlagMask::NONE and `cpu.flags` keeps the previous live value. So per-op
            // flag comparison diverges on nearly every op — it measures elision, not a
            // bug. A wrongly-elided flag that is actually consumed by adc/sbb/adcx/adox
            // would corrupt the GPR result, which the data pass compares exactly (clean);
            // the only bug this can't catch is a flag consumed by a conditional BRANCH.
            // Chasing that needs branch-point instrumentation (verify each jcc's taken
            // direction against hardware), not this per-op replay. Kept as scaffolding.
            if cmp_flags {
                // Compare only flags this instruction leaves architecturally DEFINED —
                // drop the ones iced marks undefined, whose value legitimately differs
                // between our interp and a specific host CPU. What remains catches a
                // wrong DEFINED flag (or a wrongly-clobbered preserved flag) that could
                // drive a wrong branch.
                use iced_x86::RflagsBits;
                let mut dec = Decoder::with_ip(64, bytes, addr, DecoderOptions::NONE);
                let mut insn = Instruction::default();
                dec.decode_out(&mut insn);
                let undef = insn.rflags_undefined();
                let mut umask = 0u64;
                for (bit, pos) in [
                    (RflagsBits::CF, 0),
                    (RflagsBits::PF, 2),
                    (RflagsBits::ZF, 6),
                    (RflagsBits::SF, 7),
                    (RflagsBits::OF, 11),
                ] {
                    if undef & bit != 0 {
                        umask |= 1u64 << pos;
                    }
                }
                let cmask = FLAG_MASK & !umask;
                if (out.cpu.flags.to_rflags() ^ post.flags) & cmask != 0 {
                    report(format!(
                        "flags: interp {:#06x} vs native {:#06x} (defined mask {:#06x})",
                        post.flags & cmask,
                        out.cpu.flags.to_rflags() & cmask,
                        cmask
                    ));
                }
            }
            if has_mem {
                if let Some(c) = out.mem.iter().find(|c| c.addr == ea) {
                    let n = c.bytes.len().min(MEMB);
                    if c.bytes[..n] != post.mem[..n] {
                        report(format!(
                            "mem@{ea:#x}: interp {:02x?} vs native {:02x?}",
                            &post.mem[..n],
                            &c.bytes[..n]
                        ));
                    }
                }
            }
        }
        eprintln!(
            "no divergence: {total} scanned, {replayed} replayed, {skipped} skipped \
             (native couldn't run), {} unique",
            seen.len()
        );
    }

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
        if host_xsave_offsets().0 == 0 {
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

    /// task-215: `vzeroall` must zero the WHOLE of ymm0–15 — the low 128 bits (xmm) as
    /// well as the upper halves — unlike `vzeroupper`, which preserves the low 128. A
    /// prior bug lifted both to the same upper-only clear, leaving xmm stale; that
    /// corrupted openssl's rsaz-avx2 crypto. Validate against the real CPU with both
    /// halves seeded non-zero so a residual low lane can't hide.
    #[test]
    fn native_vzeroall_clears_whole_register_matches_interp() {
        if host_xsave_offsets().0 == 0 {
            return; // no AVX host → no YMM state to clear/capture
        }
        let code = 0x21_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vzeroall().unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        for i in 0..16 {
            init.xmm[i] = 0xDEAD_0000_0000_0000_0000_0000_0000_0001 ^ (i as u128);
            init.ymm_hi[i] = 0xBEEF_0000_0000_0000_0000_0000_0000_0002 ^ (i as u128);
        }
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX host runs a vzeroall snippet");
        // Real hardware zeros the whole register file.
        assert!(
            native.cpu.xmm.iter().all(|&x| x == 0) && native.cpu.ymm_hi.iter().all(|&h| h == 0),
            "vzeroall must zero xmm AND ymm_hi on hardware: xmm={:x?} ymm_hi={:x?}",
            native.cpu.xmm,
            native.cpu.ymm_hi
        );
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interp diverges from hardware on vzeroall:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-215: 16-bit `movbe` store/load validated against the real CPU. The interp
    /// byte-swapped 32 bits for a 16-bit operand (wrong), corrupting openssl's PEM/base64
    /// key decode -> wrong RSA signatures. Both halves of the value must round-trip.
    #[test]
    fn native_movbe16_matches_interp() {
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut spage = vec![0u8; 0x1000];
        spage[..8].copy_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());

        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(rax, scratch).unwrap();
        a.movbe(cx, word_ptr(rax)).unwrap(); // 16-bit byte-swap load
        a.movbe(word_ptr(rax + 16), cx).unwrap(); // 16-bit byte-swap store
        a.movbe(edx, dword_ptr(rax)).unwrap(); // 32-bit form
        a.movbe(dword_ptr(rax + 24), edx).unwrap();
        a.movbe(r8, qword_ptr(rax)).unwrap(); // 64-bit form
        a.movbe(qword_ptr(rax + 32), r8).unwrap();
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
                    bytes: spage,
                    kind: MemKind::Ram,
                },
            ],
            entry: code,
            run: RunSpec::UntilExit,
        };
        let native = run_native(&input).expect("host runs a movbe snippet");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interp diverges from hardware on movbe:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-215: `vpermilps`/`vpermilpd` (imm8, VEX.128) validated against the real CPU —
    /// reg and memory source. openssl's rsaz-avx2 keygen emits the memory-source
    /// `vpermilpd`; a shared interp/JIT lowering bug (like vzeroall) would pass jit==interp
    /// but is caught here against hardware.
    #[test]
    fn native_vpermil_imm_match_interp() {
        if host_xsave_offsets().0 == 0 {
            return; // vpermil needs AVX
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        // Distinct 16 bytes so every element permutation is observable.
        let mut spage = vec![0u8; 0x1000];
        spage[..16].copy_from_slice(&(0..16u8).collect::<Vec<_>>());
        let src = u128::from_le_bytes(spage[..16].try_into().unwrap());

        let mut a = CodeAssembler::new(64).unwrap();
        a.vpermilps(xmm1, xmm0, 0b00_01_10_11i32).unwrap(); // reg, reversed dwords
        a.vpermilpd(xmm2, xmm0, 0b01i32).unwrap(); // reg, swap doubles
        a.mov(rax, scratch).unwrap();
        a.vpermilpd(xmm3, xmmword_ptr(rax), 0b10i32).unwrap(); // mem source (rsaz form)
        a.vpermilps(xmm4, xmmword_ptr(rax), 0b11_00_11_00i32)
            .unwrap(); // mem source
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[0] = src;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![
                MemChunk {
                    addr: code,
                    bytes,
                    kind: MemKind::Ram,
                },
                MemChunk {
                    addr: scratch,
                    bytes: spage,
                    kind: MemKind::Ram,
                },
            ],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX host runs a vpermil snippet");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interp diverges from hardware on vpermil:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.1: the EVEX masked compare `vpcmpeqb k, xmm, xmm` — glibc's heaviest
    /// task-168.5.4: EVEX `vptestnmb` (glibc's AVX-512 strlen zero-byte probe) validated
    /// against the real CPU — the interpreter's `(a & b) == 0` per-byte mask must match
    /// hardware. Self-skips without AVX-512VL.
    #[test]
    fn native_vptestnmb_matches_interp() {
        if !std::is_x86_feature_detected!("avx512vl") {
            return;
        }
        let code = 0x21_0000u64;
        // Bytes where (a & b) == 0 → mask bit set. b = 0x0F mask; a's low nibble 0 in
        // lanes 2 and 5 → those bits set.
        let x0: u128 = 0xF1F2_F3F4_F5F6_F7F8_F9FA_F0FB_FC00_FDF0;
        let x1: u128 = 0x0F0F_0F0F_0F0F_0F0F_0F0F_0F0F_0F0F_0F0F;

        let mut a = CodeAssembler::new(64).unwrap();
        a.vptestnmb(k1, xmm0, xmm1).unwrap();
        a.kmovd(eax, k1).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[0] = x0;
        init.xmm[1] = x1;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };
        let native = run_native(&input).expect("AVX-512VL host runs vptestnmb");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vptestnmb:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// AVX-512 op — validated against the **real CPU**. Unicorn can't decode EVEX, so
    /// this is the only automatic check that the interpreter's opmask semantics match
    /// hardware (not just that the JIT mirrors the interpreter). The mask is moved to a
    /// GPR so the captured state carries it. Self-skips on a host without AVX-512.
    #[test]
    fn native_evex_vpcmpeqb_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") {
            return;
        }
        let code = 0x21_0000u64;

        // Byte lane 2 differs (0x02 vs 0xff); the other 15 are equal → mask 0xFFFB.
        let x0: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
        let x1: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_03ff_0100;

        let mut a = CodeAssembler::new(64).unwrap();
        // xmm0/xmm1 come from the init snapshot; compare and pull the mask into eax.
        a.vpcmpeqb(k1, xmm0, xmm1).unwrap();
        a.kmovd(eax, k1).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[0] = x0;
        init.xmm[1] = x1;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX-512 host runs an EVEX vpcmpeqb snippet");
        // 15 equal byte lanes (all but lane 2) → mask 0xFFFB.
        assert_eq!(native.cpu.gpr[0], 0xFFFB, "vpcmpeqb mask (real CPU)");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on EVEX vpcmpeqb:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: memory-source `src2` for the EVEX mask compares (`vpcmpeqb k, ymm, [mem]`,
    /// `vpcmp[u]d`, `vptestnmb`) validated against the real CPU. glibc folds the second
    /// operand as a load; this is the only automatic check that the memory-source path's
    /// opmask semantics match hardware (Unicorn can't decode EVEX). Both operands are staged
    /// in scratch (a nonzero YMM init is rejected); the compare reads B straight from memory.
    #[test]
    fn native_evex_vpcmp_mem_src_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        // A (loaded into ymm0) vs B (kept in memory): byte lane 2 differs; dword ordering
        // gives a nontrivial signed-GT / unsigned-LT mask too.
        let a_bytes: Vec<u8> = (0..32u8).collect();
        let mut b_bytes = a_bytes.clone();
        b_bytes[2] = 0xFF; // lane 2 differs

        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu(ymm0, ymmword_ptr(scratch)).unwrap(); // ymm0 = A
        a.vpcmpeqb(k1, ymm0, ymmword_ptr(scratch + 32)).unwrap(); // 256-bit, byte lanes
        a.kmovd(eax, k1).unwrap();
        a.vpcmpd(k2, xmm0, xmmword_ptr(scratch + 32), 6).unwrap(); // signed GT, dwords
        a.kmovd(edx, k2).unwrap();
        a.vptestnmb(k3, xmm0, xmmword_ptr(scratch + 32)).unwrap(); // (a & b) == 0 per byte
        a.kmovd(ecx, k3).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..32].copy_from_slice(&a_bytes);
        scratch_page[32..64].copy_from_slice(&b_bytes);
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

        let native = run_native(&input).expect("AVX-512 host runs EVEX vpcmp with a memory src");
        // 31 of 32 byte lanes equal (all but lane 2) → mask has bit 2 clear.
        assert_eq!(
            native.cpu.gpr[0], 0xFFFF_FFFB,
            "vpcmpeqb mem-src mask (real CPU)"
        );
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on EVEX vpcmp mem-src:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: AVX-512 ops the real v4 `sort` binary uses — per-lane popcount
    /// `vpopcnt{d,q}` and the two-table permute `vpermt2d` — validated against the real CPU.
    /// Inputs are staged in scratch (a nonzero ZMM init is rejected). Self-skips without
    /// AVX512F + VPOPCNTDQ (the popcount half; the permute needs only AVX512F).
    #[test]
    fn native_vpopcnt_vpermt2_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f")
            || !std::is_x86_feature_detected!("avx512vpopcntdq")
        {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let pa: Vec<u8> = (0..64u8)
            .map(|b| b.wrapping_mul(37).wrapping_add(9))
            .collect();
        let ptbl: Vec<u8> = (0..64u8)
            .map(|b| b.wrapping_mul(11).wrapping_add(3))
            .collect();
        // Per-dword indices into the 32-lane {zmm2, zmm3} table.
        let mut pidx = vec![0u8; 64];
        for i in 0..16 {
            let id = ((i * 7 + 1) & 31) as u32;
            pidx[i * 4..i * 4 + 4].copy_from_slice(&id.to_le_bytes());
        }

        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm0, zmmword_ptr(scratch)).unwrap(); // A
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap(); // table0 (also result)
        a.vmovdqu64(zmm3, zmmword_ptr(scratch + 128)).unwrap(); // index
        a.vmovdqu64(zmm1, zmmword_ptr(scratch + 64)).unwrap(); // table1 = same pattern
        a.vpopcntq(zmm5, zmm0).unwrap();
        a.vpopcntd(zmm6, zmm0).unwrap();
        a.vpermt2d(zmm2, zmm3, zmm1).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..64].copy_from_slice(&pa);
        scratch_page[64..128].copy_from_slice(&ptbl);
        scratch_page[128..192].copy_from_slice(&pidx);
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

        let native = run_native(&input).expect("AVX-512 host runs vpopcnt/vpermt2d");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vpopcnt/vpermt2d:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.5: EVEX write-masked **memory** moves validated against the real CPU —
    /// `vmovdqu8 v{k}{z}, [mem]` (load, zeroing + merge) and `[mem]{k}, v` (store). Confirms
    /// the interpreter's element-wise `masked_load_run`/`masked_store_run` (incl. the merge
    /// vs zero blend) match hardware. Mask + merge base are built in-snippet (run_native
    /// rejects nonzero YMM/opmask init). Self-skips without AVX-512VL/BW.
    #[test]
    fn native_masked_mem_move_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") || !std::is_x86_feature_detected!("avx512vl")
        {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let a_bytes: Vec<u8> = (0..32u8)
            .map(|b| b.wrapping_mul(3).wrapping_add(1))
            .collect();
        let merge_bytes: Vec<u8> = (0..32u8).map(|_| 0xEE).collect();

        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(rax, scratch).unwrap();
        a.mov(ecx, 0x00A5_5A3Cu32).unwrap();
        a.kmovd(k1, ecx).unwrap(); // byte mask over the 256-bit operand
        a.vmovdqu(ymm2, ymmword_ptr(scratch + 32)).unwrap(); // merge base (in-snippet)
        a.vmovdqu8(ymm1.k1().z(), ymmword_ptr(rax)).unwrap(); // masked load, zeroing
        a.vmovdqu8(ymm2.k1(), ymmword_ptr(rax)).unwrap(); // masked load, merge
        a.vmovdqu8(ymmword_ptr(scratch + 64).k1(), ymm1).unwrap(); // masked store
        a.vmovdqu(ymm3, ymmword_ptr(scratch + 64)).unwrap(); // read store result back
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..32].copy_from_slice(&a_bytes);
        scratch_page[32..64].copy_from_slice(&merge_bytes);
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

        let native = run_native(&input).expect("AVX-512 host runs masked memory moves");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on masked memory moves:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: 512-bit memory-source EVEX data ops validated against the real CPU —
    /// `vpxorq`/`vpternlogd`/`vpaddq zmm, zmm, [mem]` (the 512-bit packed-add path was
    /// entirely unlifted) and `vpbroadcastw zmm, [mem]`. Operands are staged in scratch and
    /// folded as loads. Self-skips without AVX-512F/BW.
    #[test]
    fn native_evex_512_mem_src_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let pa: Vec<u8> = (0..64u8)
            .map(|b| b.wrapping_mul(7).wrapping_add(3))
            .collect();

        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(rax, scratch).unwrap();
        a.vmovdqu64(zmm0, zmmword_ptr(rax)).unwrap(); // zmm0 = A (512-bit)
        a.vpxorq(zmm1, zmm0, zmmword_ptr(rax)).unwrap(); // a ^ a == 0
        a.vpternlogd(zmm2, zmm0, zmmword_ptr(rax), 0x96).unwrap(); // xor3
        a.vpaddq(zmm3, zmm0, zmmword_ptr(rax)).unwrap(); // 512-bit packed add
        a.vpbroadcastw(zmm4, word_ptr(rax)).unwrap(); // broadcast low word across 512
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..64].copy_from_slice(&pa);
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

        let native = run_native(&input).expect("AVX-512 host runs 512-bit mem-src data ops");
        assert_eq!(native.cpu.zmm_hi[1], [0u128; 2], "vpxorq a^a low/high == 0");
        assert_eq!(native.cpu.xmm[1], 0, "vpxorq a^a lane 0 == 0");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on 512-bit mem-src data ops:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.2: EVEX `vpxorq` and `vpternlogd` (128-bit) validated against the real
    /// CPU. Confirms the interpreter's bitwise-logic and truth-table semantics match
    /// hardware — Unicorn can't decode EVEX, so this is the only automatic check.
    /// Self-skips on a host without AVX-512VL (the 128-bit EVEX forms).
    #[test]
    fn native_evex_logic_ternlog_matches_interp() {
        if !std::is_x86_feature_detected!("avx512vl") {
            return;
        }
        let code = 0x21_0000u64;
        let p1: u128 = 0xF0F0_F0F0_0F0F_0F0F_AAAA_5555_1234_5678;
        let p2: u128 = 0x0FF0_1234_DEAD_BEEF_5A5A_A5A5_9999_0000;

        let mut a = CodeAssembler::new(64).unwrap();
        a.vpxorq(xmm0, xmm1, xmm2).unwrap(); // xmm0 = xmm1 ^ xmm2
        a.vpternlogd(xmm3, xmm1, xmm2, 0x96).unwrap(); // xmm3 = xmm3 ^ xmm1 ^ xmm2
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[1] = p1;
        init.xmm[2] = p2;
        init.xmm[3] = p1 & p2; // ternlog's first source (dst)
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX-512VL host runs EVEX vpxorq/vpternlogd");
        assert_eq!(native.cpu.xmm[0], p1 ^ p2, "vpxorq result (real CPU)");
        assert_eq!(
            native.cpu.xmm[3],
            (p1 & p2) ^ p1 ^ p2,
            "vpternlogd 0x96 = a^b^c (real CPU)"
        );
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on EVEX logic/ternlog:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.4: SSE4.1 `pmovsxbw` (sign-extend) and `pmulld` validated against the
    /// real CPU — the interpreter's lane-extension and 32-bit-multiply semantics must
    /// match hardware. Self-skips on a host without SSE4.1 (universal on x86-64, guarded
    /// for completeness).
    #[test]
    fn native_sse41_pmovsx_pmulld_matches_interp() {
        if !std::is_x86_feature_detected!("sse4.1") {
            return;
        }
        let code = 0x21_0000u64;
        let src: u128 = 0x8000_7FFF_FE01_80FF_1234_5678_9ABC_DEF0;
        let m0: u128 = 0x0000_0002_FFFF_FFFF_0000_0003_8000_0000;
        let m1: u128 = 0x0000_0003_0000_0002_0000_0004_0000_0002;

        let mut a = CodeAssembler::new(64).unwrap();
        a.pmovsxbw(xmm0, xmm1).unwrap(); // sign-extend low 8 bytes → 8 words
        a.pmulld(xmm2, xmm3).unwrap(); // 4× 32-bit low product
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[1] = src;
        init.xmm[2] = m0;
        init.xmm[3] = m1;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("SSE4.1 host runs pmovsxbw/pmulld");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on SSE4.1 pmovsx/pmulld:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.4: `pcmpistri`/`pcmpestri` fuzzed against the real CPU across every imm8
    /// aggregation/polarity/format/sign/index-select combination. The string-compare
    /// semantics are subtle, so this hardware oracle is the real correctness check (the
    /// JIT can only confirm it mirrors the interpreter). Self-skips without SSE4.2.
    #[test]
    fn native_pcmpstr_fuzz_matches_interp() {
        if !std::is_x86_feature_detected!("sse4.2") {
            return;
        }
        // Small deterministic xorshift so inputs vary but the test is reproducible.
        let mut s: u64 = 0x1234_5678_9ABC_DEF1;
        let mut rng = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut ran = 0u64;
        for fmt in 0..2u8 {
            // format: 0=byte, 1=word
            for signed in 0..2u8 {
                for agg in 0..4u8 {
                    for pol in [0u8, 1, 3] {
                        for msb in 0..2u8 {
                            let imm = fmt | (signed << 1) | (agg << 2) | (pol << 4) | (msb << 6);
                            for _ in 0..3 {
                                // Mix in some null elements by masking random bytes to 0.
                                let mut x0 = (rng() as u128) | ((rng() as u128) << 64);
                                let mut x1 = (rng() as u128) | ((rng() as u128) << 64);
                                if rng() & 1 == 0 {
                                    x0 &= !(0xFFu128 << ((rng() % 16) * 8));
                                }
                                if rng() & 1 == 0 {
                                    x1 &= !(0xFFu128 << ((rng() % 16) * 8));
                                }
                                for estri in [false, true] {
                                    let len_a = (rng() % 20) as i32 - 4; // exercise ±/saturation
                                    let len_d = (rng() % 20) as i32 - 4;
                                    if pcmpstr_case(x0, x1, len_a, len_d, imm, estri) {
                                        ran += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(ran > 100, "pcmpstr fuzz ran too few native cases ({ran})");
    }

    /// One pcmpistri/pcmpestri case: native vs interpreter. Returns true if it ran (host
    /// executed it natively); panics on a divergence.
    fn pcmpstr_case(x0: u128, x1: u128, len_a: i32, len_d: i32, imm: u8, estri: bool) -> bool {
        let code = 0x21_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        if estri {
            a.pcmpestri(xmm0, xmm1, imm as u32).unwrap();
        } else {
            a.pcmpistri(xmm0, xmm1, imm as u32).unwrap();
        }
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[0] = x0;
        init.xmm[1] = x1;
        init.gpr[0] = len_a as u32 as u64; // EAX (src1 length for estri)
        init.gpr[2] = len_d as u32 as u64; // EDX (src2 length)
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };
        let Some(native) = run_native(&input) else {
            return false;
        };
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "pcmpstr diverges from the real CPU (imm={imm:#04x} estri={estri} \
             x0={x0:#034x} x1={x1:#034x} eax={len_a} edx={len_d}):\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
        true
    }

    /// task-168.5.6: EVEX `vinserti32x4` and `valignd` validated against the real CPU —
    /// confirms the lane-insert position and the `valign` concatenation/shift order (the
    /// risky assumption) match hardware. ZMM operands are loaded from memory in-snippet
    /// (a nonzero ZMM init is rejected), so only xmm3 comes from the init. Skips w/o AVX-512.
    #[test]
    fn native_lane_ops_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let pa: Vec<u8> = (0..64u8)
            .map(|b| b.wrapping_mul(7).wrapping_add(3))
            .collect();
        let pb: Vec<u8> = (0..64u8)
            .map(|b| b.wrapping_mul(5).wrapping_add(11))
            .collect();

        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap(); // pattern A
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap(); // pattern B
        a.vinserti32x4(zmm0, zmm1, xmm3, 2).unwrap(); // insert xmm3 into lane 2
        a.valignd(zmm4, zmm1, zmm2, 3).unwrap(); // (zmm1:zmm2) >> 3 dwords
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..64].copy_from_slice(&pa);
        scratch_page[64..128].copy_from_slice(&pb);
        let insert = 0xDEAD_BEEF_CAFE_BABE_0123_4567_89AB_CDEFu128;
        let mut init = CpuSnapshot::default();
        init.xmm[3] = insert;
        let input = VectorInput {
            cpu_init: init,
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

        let native = run_native(&input).expect("AVX-512 host runs vinserti32x4/valignd");
        // vinserti32x4 into lane 2: zmm0 lane 2 (bits 383:256, i.e. zmm_hi[0][0]) == xmm3.
        assert_eq!(
            native.cpu.zmm_hi[0][0], insert,
            "vinserti32x4 placed xmm3 into lane 2"
        );
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on EVEX lane ops:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.5: masked EVEX logic (`vpxord{k}` merge, `vpxorq{k}{z}` zero) validated
    /// against the real CPU — confirms the interpreter's `write_masked` semantics (which
    /// merge/zero-mask) match hardware. Self-skips without AVX-512VL.
    #[test]
    fn native_masked_logic_matches_interp() {
        if !std::is_x86_feature_detected!("avx512vl") {
            return;
        }
        let code = 0x21_0000u64;
        let a_pat: u128 = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        let b_pat: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
        let d_pat: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;

        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(eax, 0b1010i32).unwrap();
        a.kmovw(k1, eax).unwrap();
        a.vpxord(xmm0.k1(), xmm1, xmm2).unwrap(); // merge (dwords 1,3)
        a.vpxorq(xmm3.k1().z(), xmm1, xmm2).unwrap(); // zero
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[1] = a_pat;
        init.xmm[2] = b_pat;
        init.xmm[0] = d_pat; // merge base
        init.xmm[3] = d_pat;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX-512VL host runs masked vpxor");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on masked EVEX logic:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: EVEX widening `vpmovsxdq zmm←ymm` (source staged in scratch) + narrowing
    /// store `vpmovqd [mem]←xmm`, validated against the real CPU. Self-skips without AVX-512F.
    #[test]
    fn native_pmov_wide_narrow_mem_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu(ymm0, ymmword_ptr(scratch)).unwrap(); // 8 dwords for vpmovsxdq
        a.vpmovsxdq(zmm4, ymm0).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch)).unwrap();
        a.vpmovqd(xmmword_ptr(scratch + 64), xmm1).unwrap(); // narrow store to memory
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(32).enumerate() {
            // mix in high bit patterns so sign-extension is exercised
            *b = (i as u8).wrapping_mul(37).wrapping_add(0x81);
        }
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
        let native = run_native(&input).expect("AVX-512F host runs pmov wide + narrow store");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on pmov wide/narrow-mem:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: AVX-512DQ `vpmullq` (64-bit multiply-low) + packed abs `vpabs{b,d,q}`,
    /// validated against the real CPU. Operands staged in scratch (nonzero ZMM init is
    /// rejected). Self-skips without AVX-512DQ (vpmullq) — abs needs only AVX-512F/BW.
    #[test]
    fn native_vpmullq_vpabs_matches_interp() {
        if !std::is_x86_feature_detected!("avx512dq") || !std::is_x86_feature_detected!("avx512bw")
        {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap();
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap();
        a.vpmullq(zmm3, zmm1, zmm2).unwrap();
        a.vpabsb(zmm4, zmm1).unwrap();
        a.vpabsd(zmm5, zmm1).unwrap();
        a.vpabsq(zmm6, zmm2).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(128).enumerate() {
            *b = (i as u8).wrapping_mul(53).wrapping_add(0x81);
        }
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
        let native = run_native(&input).expect("AVX-512DQ host runs vpmullq + vpabs");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vpmullq/vpabs:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-209: masked EVEX unary lane ops `vplzcnt{d,q}` / `vprol{d,q}` /
    /// `vpconflict{d,q}` (unmasked + masked merge + zeroing), validated BIT-EXACT against
    /// the real CPU. Ground-truth for the lane function + opmask merge/zero semantics.
    /// Scratch dwords carry deliberate repeats so `vpconflict` finds real matches.
    /// Self-skips without AVX-512CD.
    #[test]
    fn native_vp_unary_lane_matches_interp() {
        if !std::is_x86_feature_detected!("avx512cd") || !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap();
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap(); // merge base
                                                               // Unmasked lane functions.
        a.vplzcntd(zmm3, zmm1).unwrap();
        a.vplzcntq(zmm4, zmm1).unwrap();
        a.vprold(zmm5, zmm1, 7).unwrap();
        a.vprolq(zmm6, zmm1, 13).unwrap();
        a.vpconflictd(zmm7, zmm1).unwrap();
        a.vpconflictq(zmm8, zmm1).unwrap();
        // Masked: merge (keep zmm2 in masked-off lanes) + zeroing.
        a.mov(eax, 0x0000_cc33u32).unwrap();
        a.kmovd(k1, eax).unwrap();
        a.vmovdqa64(zmm9, zmm2).unwrap();
        a.vplzcntd(zmm9.k1(), zmm1).unwrap(); // merge
        a.vprold(zmm10.k1().z(), zmm1, 3).unwrap(); // zeroing
        a.vpconflictd(zmm11.k1().z(), zmm1).unwrap(); // zeroing
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // 16 dwords cycling through 3 distinct values → guaranteed conflict matches, plus
        // varied bytes so lzcnt/rol lanes differ. Second 64 bytes = merge base.
        for (i, b) in scratch_page.iter_mut().take(128).enumerate() {
            let dword = i / 4;
            *b = ((dword % 3) as u8)
                .wrapping_mul(0x40)
                .wrapping_add((i % 4) as u8 * 0x11)
                .wrapping_add(1);
        }
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
        let native = run_native(&input).expect("AVX-512CD host runs vplzcnt/vprol/vpconflict");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vp_unary_lane:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-209: masked EVEX blend `vpblendm{d,q}` (merge + zeroing), validated BIT-EXACT
    /// against the real CPU. Ground-truth for the opmask blend-control semantics.
    /// Self-skips without AVX-512F.
    #[test]
    fn native_vp_blendm_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap();
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap();
        a.mov(eax, 0x0000_a5c3u32).unwrap();
        a.kmovd(k1, eax).unwrap();
        a.vpblendmd(zmm3.k1(), zmm1, zmm2).unwrap(); // dword blend, merge (a on off-lanes)
        a.vpblendmq(zmm4.k1().z(), zmm1, zmm2).unwrap(); // qword blend, zeroing
        a.vpblendmd(zmm5.k1().z(), zmm1, zmm2).unwrap(); // dword blend, zeroing
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(128).enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(0x11);
        }
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
        let native = run_native(&input).expect("AVX-512F host runs vpblendm");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vpblendm:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-209: masked EVEX 128-bit-lane shuffle `vshuff32x4` / `vshuff64x2` (512 + 256,
    /// unmasked + masked merge + zeroing), validated BIT-EXACT against the real CPU.
    /// Ground-truth for the imm8 lane selection + masking. Self-skips without AVX-512F.
    #[test]
    fn native_vshuf_lane_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap();
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap();
        a.vshuff32x4(zmm3, zmm1, zmm2, 0b11_01_10_00).unwrap();
        a.vshuff64x2(zmm4, zmm1, zmm2, 0b00_11_01_10).unwrap();
        a.vshuff32x4(ymm5, ymm1, ymm2, 0b11).unwrap(); // 256-bit: 2 lanes
        a.mov(eax, 0x0000_ff0fu32).unwrap();
        a.kmovd(k1, eax).unwrap();
        a.vmovdqa64(zmm6, zmm2).unwrap();
        a.vshuff32x4(zmm6.k1(), zmm1, zmm2, 0x1b).unwrap(); // merge
        a.vshuff32x4(zmm7.k1().z(), zmm1, zmm2, 0x1b).unwrap(); // zeroing
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(128).enumerate() {
            *b = (i as u8).wrapping_mul(47).wrapping_add(0x23);
        }
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
        let native = run_native(&input).expect("AVX-512F host runs vshuff32x4/64x2");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vshuf_lane:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-209: masked EVEX `vpmultishiftqb` (VBMI, unmasked + masked zeroing), validated
    /// BIT-EXACT against the real CPU. Ground-truth for the per-qword unaligned byte gather
    /// (control byte → 6-bit rotate) + operand order. Self-skips without AVX-512-VBMI.
    #[test]
    fn native_vp_multishift_matches_interp() {
        if !std::is_x86_feature_detected!("avx512vbmi") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap(); // control (shift indices)
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap(); // data
        a.vpmultishiftqb(zmm3, zmm1, zmm2).unwrap();
        a.mov(rax, 0x0f0f_0f0f_ffff_0000u64).unwrap();
        a.kmovq(k1, rax).unwrap();
        a.vpmultishiftqb(zmm4.k1().z(), zmm1, zmm2).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // Control bytes span 0..63 shifts; data has varied bit patterns.
        for (i, b) in scratch_page.iter_mut().take(64).enumerate() {
            *b = (i as u8).wrapping_mul(7); // control: 0,7,14,... mod 256
        }
        for (i, b) in scratch_page.iter_mut().skip(64).take(64).enumerate() {
            *b = (i as u8).wrapping_mul(53).wrapping_add(0x81); // data
        }
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
        let native = run_native(&input).expect("AVX-512-VBMI host runs vpmultishiftqb");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vpmultishiftqb:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: EVEX-512 `vpshufb zmm` per-lane byte shuffle (unmasked + masked),
    /// validated against the real CPU. Operands staged in scratch (nonzero ZMM init is
    /// rejected). Self-skips without AVX-512BW.
    #[test]
    fn native_vpshufb_wide_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm0, zmmword_ptr(scratch)).unwrap(); // data
        a.vmovdqu64(zmm1, zmmword_ptr(scratch + 64)).unwrap(); // control
        a.mov(rax, 0x0F0F_0F0F_0F0F_0F0Fu64 as i64).unwrap();
        a.kmovq(k1, rax).unwrap();
        a.vpshufb(zmm4, zmm0, zmm1).unwrap();
        a.vpshufb(zmm5.k1().z(), zmm0, zmm1).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(64).enumerate() {
            *b = (i as u8).wrapping_mul(29).wrapping_add(3);
        }
        // control: per-byte selector, some with the MSB set (→ zero)
        for i in 0..64usize {
            scratch_page[64 + i] = if i % 5 == 0 {
                0x80
            } else {
                ((i as u8).wrapping_mul(7)) & 0x0F
            };
        }
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
        let native = run_native(&input).expect("AVX-512BW host runs vpshufb zmm");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vpshufb zmm:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-201: FMA3 `vfmadd/vfmsub/vfnmadd/vfnmsub` (132/213/231, scalar sd + packed pd),
    /// validated against the real CPU. Operands staged in scratch. Self-skips without FMA.
    #[test]
    fn native_fma_matches_interp() {
        if !std::is_x86_feature_detected!("fma") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovupd(xmm0, xmmword_ptr(scratch)).unwrap(); // [x0, x1]
        a.vmovupd(xmm1, xmmword_ptr(scratch + 16)).unwrap(); // [y0, y1]
        a.vmovupd(xmm2, xmmword_ptr(scratch + 32)).unwrap(); // [z0, z1]
                                                             // scalar sd: exercise all three orders + a memory operand
        a.vmovapd(xmm3, xmm0).unwrap();
        a.vfmadd132sd(xmm3, xmm2, xmm1).unwrap(); // xmm3 = xmm3*xmm1 + xmm2
        a.vmovapd(xmm4, xmm0).unwrap();
        a.vfmadd213sd(xmm4, xmm1, xmm2).unwrap(); // xmm4 = xmm1*xmm4 + xmm2
        a.vmovapd(xmm5, xmm0).unwrap();
        a.vfmadd231sd(xmm5, xmm1, xmmword_ptr(scratch + 32))
            .unwrap(); // + mem
                       // sign variants (packed pd, 213)
        a.vmovupd(xmm6, xmm0).unwrap();
        a.vfmsub213pd(xmm6, xmm1, xmm2).unwrap();
        a.vmovupd(xmm7, xmm0).unwrap();
        a.vfnmadd213pd(xmm7, xmm1, xmm2).unwrap();
        a.vmovupd(xmm8, xmm0).unwrap();
        a.vfnmsub231pd(xmm8, xmm1, xmm2).unwrap();
        // scalar single (ss) + packed single (ps) + a packed-pd memory operand
        a.vmovaps(xmm9, xmm0).unwrap();
        a.vfmadd132ss(xmm9, xmm2, xmm1).unwrap();
        a.vmovaps(xmm10, xmm0).unwrap();
        a.vfmsub231ps(xmm10, xmm1, xmm2).unwrap();
        a.vmovaps(xmm11, xmm0).unwrap();
        a.vfnmadd213ps(xmm11, xmm1, xmmword_ptr(scratch + 32))
            .unwrap(); // ps mem
        a.vmovupd(xmm12, xmm0).unwrap();
        a.vfmadd132pd(xmm12, xmm2, xmmword_ptr(scratch + 16))
            .unwrap(); // pd mem
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        let vals: [f64; 6] = [1.5, -2.25, 3.0, 0.5, -4.0, 2.5];
        for (i, v) in vals.iter().enumerate() {
            scratch_page[i * 8..i * 8 + 8].copy_from_slice(&v.to_bits().to_le_bytes());
        }
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
        let native = run_native(&input).expect("FMA host runs vfmadd/sub");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on FMA:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-261: FMA alternating-sign `vfmaddsub`/`vfmsubadd{132,213,231}{ps,pd}` (xmm +
    /// ymm, reg + mem), validated BIT-EXACT against the real CPU — the fused single rounding
    /// AND the per-lane even/odd sign must match hardware. NaN/rounding-sensitive operands
    /// seeded. Self-skips without FMA or host xsave.
    #[test]
    fn native_fma_addsub_matches_interp() {
        if !std::is_x86_feature_detected!("fma") || host_xsave_offsets().0 == 0 {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        // pd operands (with NaN) at 0/32/64; finite ps operands at 96/128/160.
        a.vmovupd(ymm0, ymmword_ptr(scratch)).unwrap(); // x (pd)
        a.vmovupd(ymm1, ymmword_ptr(scratch + 32)).unwrap(); // y (pd)
        a.vmovupd(ymm2, ymmword_ptr(scratch + 64)).unwrap(); // z (pd)
        a.vmovups(ymm13, ymmword_ptr(scratch + 96)).unwrap(); // x (ps)
        a.vmovups(ymm14, ymmword_ptr(scratch + 128)).unwrap(); // y (ps)
        a.vmovups(ymm15, ymmword_ptr(scratch + 160)).unwrap(); // z (ps)
                                                               // packed pd (xmm), both families, three orders — NaN propagation
        a.vmovapd(xmm3, xmm0).unwrap();
        a.vfmaddsub132pd(xmm3, xmm2, xmm1).unwrap();
        a.vmovapd(xmm4, xmm0).unwrap();
        a.vfmaddsub213pd(xmm4, xmm1, xmm2).unwrap();
        a.vmovapd(xmm5, xmm0).unwrap();
        a.vfmsubadd231pd(xmm5, xmm1, xmm2).unwrap();
        // packed ps (xmm), both families — finite, rounding-sensitive
        a.vmovaps(xmm6, xmm13).unwrap();
        a.vfmaddsub213ps(xmm6, xmm14, xmm15).unwrap();
        a.vmovaps(xmm7, xmm13).unwrap();
        a.vfmsubadd213ps(xmm7, xmm14, xmm15).unwrap();
        // ymm (per-128-lane sign), both families + a ps memory operand
        a.vmovupd(ymm8, ymm0).unwrap();
        a.vfmaddsub231pd(ymm8, ymm1, ymm2).unwrap();
        a.vmovups(ymm9, ymm13).unwrap();
        a.vfmsubadd231ps(ymm9, ymm14, ymmword_ptr(scratch + 160))
            .unwrap(); // ps ymm mem
        a.vmovapd(xmm10, xmm0).unwrap();
        a.vfmaddsub231pd(xmm10, xmm1, xmmword_ptr(scratch + 64))
            .unwrap(); // pd xmm mem
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // Rounding-sensitive finite operands near ULP boundaries: the whole point is the
        // *fused single rounding* (a separate mul+add would round differently). Raw NaNs
        // are deliberately NOT seeded here — an FMA's NaN *sign* bit is architecturally
        // unspecified and legitimately differs between the softfloat path and hardware; the
        // jit==interp test covers NaN operands (both sides share the softfloat path).
        let xs: [f64; 4] = [1.0 + 2f64.powi(-52), -3.5, 1.0 + 2f64.powi(-51), 2.0];
        let ys: [f64; 4] = [1.0 - 2f64.powi(-53), 7.25, 1.5, -0.5];
        let zs: [f64; 4] = [2f64.powi(-60), 0.125, -9.0, 2f64.powi(-55)];
        // ps: 8 finite f32 lanes each, rounding-sensitive (values near ULP boundaries).
        let xps: [f32; 8] = [
            1.0 + 2f32.powi(-23),
            -3.5,
            6.25,
            -0.5,
            2.0,
            -7.75,
            1.5,
            -0.125,
        ];
        let yps: [f32; 8] = [1.0 - 2f32.powi(-24), 7.25, 1.5, -0.5, -2.5, 3.0, -1.25, 8.0];
        let zps: [f32; 8] = [2f32.powi(-30), 0.125, -9.0, 4.5, -6.0, 0.75, -2.25, 5.5];
        for (i, v) in xps.iter().enumerate() {
            scratch_page[96 + i * 4..96 + i * 4 + 4].copy_from_slice(&v.to_bits().to_le_bytes());
        }
        for (i, v) in yps.iter().enumerate() {
            scratch_page[128 + i * 4..128 + i * 4 + 4].copy_from_slice(&v.to_bits().to_le_bytes());
        }
        for (i, v) in zps.iter().enumerate() {
            scratch_page[160 + i * 4..160 + i * 4 + 4].copy_from_slice(&v.to_bits().to_le_bytes());
        }
        for (i, v) in xs.iter().enumerate() {
            scratch_page[i * 8..i * 8 + 8].copy_from_slice(&v.to_bits().to_le_bytes());
        }
        for (i, v) in ys.iter().enumerate() {
            scratch_page[32 + i * 8..32 + i * 8 + 8].copy_from_slice(&v.to_bits().to_le_bytes());
        }
        for (i, v) in zs.iter().enumerate() {
            scratch_page[64 + i * 8..64 + i * 8 + 8].copy_from_slice(&v.to_bits().to_le_bytes());
        }
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
        let native = run_native(&input).expect("FMA host runs vfmaddsub/vfmsubadd");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on FMA add-sub/sub-add:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-261: VEX.256 float horizontal `vh{add,sub}p{s,d}` / `vaddsubp{s,d}` in the
    /// `ymm,ymm,ymm/m256` form (per-128-lane), validated against the real CPU. Reg + m256
    /// source. Self-skips without AVX or host xsave.
    #[test]
    fn native_hadd_addsub_ymm_matches_interp() {
        if !std::is_x86_feature_detected!("avx") || host_xsave_offsets().0 == 0 {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovupd(ymm0, ymmword_ptr(scratch)).unwrap();
        a.vmovupd(ymm1, ymmword_ptr(scratch + 32)).unwrap();
        a.vhaddpd(ymm4, ymm0, ymm1).unwrap();
        a.vhsubpd(ymm5, ymm0, ymm1).unwrap();
        a.vaddsubpd(ymm6, ymm0, ymm1).unwrap();
        a.vhaddps(ymm7, ymm0, ymm1).unwrap();
        a.vhsubps(ymm8, ymm0, ymm1).unwrap();
        a.vaddsubps(ymm9, ymm0, ymm1).unwrap();
        a.vhaddpd(ymm10, ymm0, ymmword_ptr(scratch + 32)).unwrap();
        a.vaddsubps(ymm11, ymm0, ymmword_ptr(scratch + 32)).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        let vals: [f32; 16] = [
            1.5, -2.25, 3.0, 0.5, -4.0, 2.5, 6.25, -7.0, 8.5, -9.75, 10.0, 0.125, -11.5, 12.0,
            -0.5, 13.25,
        ];
        for (i, v) in vals.iter().enumerate() {
            scratch_page[i * 4..i * 4 + 4].copy_from_slice(&v.to_bits().to_le_bytes());
        }
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
        let native = run_native(&input).expect("AVX host runs ymm horizontal float");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on ymm horizontal float:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-214: EVEX lane broadcast `vbroadcast{i,f}{32x4,64x2,32x8,64x4}` (128/256-bit
    /// chunk replicated across the dest) — reg + memory chunk, unmasked + masked merge +
    /// zeroing — validated BIT-EXACT against the real CPU. openssl's v4 PRNG hits
    /// `vbroadcasti64x2`. Self-skips without AVX-512DQ.
    #[test]
    fn native_broadcast_lane_matches_interp() {
        if !std::is_x86_feature_detected!("avx512dq") || !std::is_x86_feature_detected!("avx512vl")
        {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap(); // merge base
        a.mov(eax, 0x0000_00a5u32).unwrap();
        a.kmovd(k1, eax).unwrap();
        // 128-bit chunk → ymm (2 lanes) / zmm (4 lanes), from memory (iced's assembler
        // exposes only the memory-source form; the register form shares the same core).
        a.vbroadcasti64x2(ymm3, xmmword_ptr(scratch)).unwrap();
        a.vbroadcasti32x4(zmm4, xmmword_ptr(scratch)).unwrap();
        a.vbroadcastf64x2(ymm5, xmmword_ptr(scratch)).unwrap();
        // 256-bit chunk → zmm (2 lanes).
        a.vbroadcasti64x4(zmm6, ymmword_ptr(scratch)).unwrap();
        a.vbroadcastf32x8(zmm7, ymmword_ptr(scratch)).unwrap();
        // Masked: merge (keep zmm1) + zeroing.
        a.vmovdqa64(zmm8, zmm1).unwrap();
        a.vbroadcasti32x4(zmm8.k1(), xmmword_ptr(scratch)).unwrap(); // merge
        a.vbroadcasti64x2(zmm9.k1().z(), xmmword_ptr(scratch))
            .unwrap(); // zeroing
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(64).enumerate() {
            *b = (i as u8).wrapping_mul(41).wrapping_add(0x13);
        }
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
        let native = run_native(&input).expect("AVX-512DQ host runs vbroadcast lane");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on lane broadcast:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-201 AC#3: masked EVEX packed FMA `vfmadd/vfmsub/vfnmadd{132,213,231}{ps,pd}`
    /// with a write-mask (merge + zeroing) at 128/256/512-bit, validated BIT-EXACT against
    /// the real CPU. Ground-truth for the per-lane mask + fused rounding. Operands + merge
    /// base staged in scratch; the k-register is built in-snippet. Self-skips without
    /// AVX-512F/VL.
    #[test]
    fn native_fma_masked_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") || !std::is_x86_feature_detected!("avx512vl") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovupd(zmm0, zmmword_ptr(scratch)).unwrap(); // x
        a.vmovupd(zmm1, zmmword_ptr(scratch + 64)).unwrap(); // y
        a.vmovupd(zmm2, zmmword_ptr(scratch + 128)).unwrap(); // z / merge base
        a.mov(eax, 0x0000_00a5u32).unwrap();
        a.kmovd(k1, eax).unwrap();
        // 512-bit pd merge + zeroing; 256-bit ps; 128-bit pd; sign variants.
        a.vmovapd(zmm3, zmm2).unwrap();
        a.vfmadd132pd(zmm3.k1(), zmm1, zmm2).unwrap(); // merge
        a.vfmadd213pd(zmm4.k1().z(), zmm1, zmm2).unwrap(); // zeroing
        a.vmovaps(ymm5, ymm2).unwrap();
        a.vfmsub231ps(ymm5.k1(), ymm1, ymm2).unwrap(); // 256-bit ps merge
        a.vfnmadd213ps(ymm6.k1().z(), ymm1, ymm2).unwrap(); // 256-bit ps zeroing
        a.vmovapd(xmm7, xmm2).unwrap();
        a.vfnmsub231pd(xmm7.k1(), xmm1, xmm2).unwrap(); // 128-bit pd merge
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // 24 f64 lanes of varied non-trivial values across x/y/z.
        for i in 0..24 {
            let v = (i as f64) * 0.5 - 5.0 + if i % 3 == 0 { 0.125 } else { -0.25 };
            scratch_page[i * 8..i * 8 + 8].copy_from_slice(&v.to_bits().to_le_bytes());
        }
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
        let native = run_native(&input).expect("AVX-512 host runs masked FMA");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on masked FMA:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-205: AES-NI `aesenc/aesdec/aesenclast/aesdeclast/aesimc/aeskeygenassist`
    /// (SSE) plus VEX.128 `vaesenc/vaesdec/vaesenclast/vaesdeclast/vaesimc/
    /// vaeskeygenassist`, validated BIT-EXACT against the real CPU (host has AES-NI).
    /// This is the ground-truth check for the S-box / GF(2^8) math / byte order.
    /// State + key staged in scratch. Self-skips without AES-NI.
    #[test]
    fn native_aes_matches_interp() {
        if !std::is_x86_feature_detected!("aes") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch)).unwrap(); // state
        a.movdqu(xmm2, xmmword_ptr(scratch + 16)).unwrap(); // round key
                                                            // SSE in-place: copy state into each dst first so the op is a clean f(state, key).
        a.movdqa(xmm0, xmm1).unwrap();
        a.aesenc(xmm0, xmm2).unwrap();
        a.movdqa(xmm3, xmm1).unwrap();
        a.aesdec(xmm3, xmm2).unwrap();
        a.movdqa(xmm4, xmm1).unwrap();
        a.aesenclast(xmm4, xmm2).unwrap();
        a.movdqa(xmm5, xmm1).unwrap();
        a.aesdeclast(xmm5, xmm2).unwrap();
        a.aesimc(xmm6, xmm1).unwrap();
        a.aeskeygenassist(xmm7, xmm1, 0x1b).unwrap();
        // SSE memory-key form (aesenc reads key from [scratch+16]).
        a.movdqa(xmm8, xmm1).unwrap();
        a.aesenc(xmm8, xmmword_ptr(scratch + 16)).unwrap();
        // VEX.128 3-operand (dst distinct, register + memory key).
        a.vaesenc(xmm9, xmm1, xmm2).unwrap();
        a.vaesdec(xmm10, xmm1, xmm2).unwrap();
        a.vaesenclast(xmm11, xmm1, xmm2).unwrap();
        a.vaesdeclast(xmm12, xmm1, xmm2).unwrap();
        a.vaesimc(xmm13, xmm1).unwrap();
        a.vaeskeygenassist(xmm14, xmm1, 0x2a).unwrap();
        a.vaesenc(xmm15, xmm1, xmmword_ptr(scratch + 16)).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // FIPS-197 Appendix B round-1 state + round key (known-answer), plus arbitrary
        // upper bytes so every S-box lane is exercised non-trivially.
        let state: u128 = 0x08_2a_2b_be_48_8d_e2_e3_f8_c6_f4_3d_e9_9a_a0_19;
        let key: u128 = 0x05_39_b1_17_76_39_2c_fe_6c_a3_54_fa_2a_23_88_a0;
        scratch_page[0..16].copy_from_slice(&state.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&key.to_le_bytes());
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
        let native = run_native(&input).expect("AES-NI host runs aes*/vaes*");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on AES-NI:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-207: SHA-NI `sha256rnds2/sha256msg1/sha256msg2` + `sha1rnds4/sha1nexte/
    /// sha1msg1/sha1msg2`, validated BIT-EXACT against the real CPU (host has SHA-NI).
    /// This is the ground-truth check for the round math / dword layout / imm→f mapping.
    /// State + message staged in scratch; `xmm0` seeded for `sha256rnds2`'s implicit W+K.
    /// Self-skips without SHA-NI.
    #[test]
    fn native_sha_matches_interp() {
        if !std::is_x86_feature_detected!("sha") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch)).unwrap(); // state / A..D
        a.movdqu(xmm2, xmmword_ptr(scratch + 16)).unwrap(); // second source (msg / W+K)
        a.movdqu(xmm0, xmmword_ptr(scratch + 32)).unwrap(); // implicit W+K for sha256rnds2
                                                            // SHA-256: copy state into each dst first, then run the op.
        a.movdqa(xmm3, xmm1).unwrap();
        a.sha256rnds2(xmm3, xmm2).unwrap(); // uses xmm0 implicitly
        a.movdqa(xmm4, xmm1).unwrap();
        a.sha256msg1(xmm4, xmm2).unwrap();
        a.movdqa(xmm5, xmm1).unwrap();
        a.sha256msg2(xmm5, xmm2).unwrap();
        // SHA-1: all four imm-selected round functions + msg/nexte helpers.
        a.movdqa(xmm6, xmm1).unwrap();
        a.sha1rnds4(xmm6, xmm2, 0u32).unwrap();
        a.movdqa(xmm7, xmm1).unwrap();
        a.sha1rnds4(xmm7, xmm2, 1u32).unwrap();
        a.movdqa(xmm8, xmm1).unwrap();
        a.sha1rnds4(xmm8, xmm2, 2u32).unwrap();
        a.movdqa(xmm9, xmm1).unwrap();
        a.sha1rnds4(xmm9, xmm2, 3u32).unwrap();
        a.movdqa(xmm10, xmm1).unwrap();
        a.sha1nexte(xmm10, xmm2).unwrap();
        a.movdqa(xmm11, xmm1).unwrap();
        a.sha1msg1(xmm11, xmm2).unwrap();
        a.movdqa(xmm12, xmm1).unwrap();
        a.sha1msg2(xmm12, xmm2).unwrap();
        // Memory second-source forms.
        a.movdqa(xmm13, xmm1).unwrap();
        a.sha256rnds2(xmm13, xmmword_ptr(scratch + 16)).unwrap();
        a.movdqa(xmm14, xmm1).unwrap();
        a.sha1rnds4(xmm14, xmmword_ptr(scratch + 16), 2u32).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // Arbitrary but non-trivial state / message / W+K (every dword exercised).
        let state: u128 = 0x6a09_e667_bb67_ae85_3c6e_f372_a54f_f53a;
        let src: u128 = 0x510e_527f_9b05_688c_1f83_d9ab_5be0_cd19;
        let wk0: u128 = 0x0000_0000_0000_0000_7137_4491_428a_2f98;
        scratch_page[0..16].copy_from_slice(&state.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&src.to_le_bytes());
        scratch_page[32..48].copy_from_slice(&wk0.to_le_bytes());
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
        let native = run_native(&input).expect("SHA-NI host runs sha256*/sha1*");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on SHA-NI:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-210: SSSE3 `psign{b,w,d}` + VEX.128 `vpsign{b,w,d}`, validated BIT-EXACT
    /// against the real CPU (SSSE3 is always present). Ground-truth check for the
    /// per-element negate/zero/keep semantics and lane widths. Src + ctrl in scratch.
    #[test]
    fn native_psign_matches_interp() {
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch)).unwrap(); // src
        a.movdqu(xmm2, xmmword_ptr(scratch + 16)).unwrap(); // ctrl
                                                            // SSE in-place: copy src into each dst, then apply f(src, ctrl).
        a.movdqa(xmm0, xmm1).unwrap();
        a.psignb(xmm0, xmm2).unwrap();
        a.movdqa(xmm3, xmm1).unwrap();
        a.psignw(xmm3, xmm2).unwrap();
        a.movdqa(xmm4, xmm1).unwrap();
        a.psignd(xmm4, xmm2).unwrap();
        // SSE memory-ctrl form.
        a.movdqa(xmm5, xmm1).unwrap();
        a.psignb(xmm5, xmmword_ptr(scratch + 16)).unwrap();
        // VEX.128 3-operand (dst distinct, register + memory ctrl).
        a.vpsignb(xmm9, xmm1, xmm2).unwrap();
        a.vpsignw(xmm10, xmm1, xmm2).unwrap();
        a.vpsignd(xmm11, xmm1, xmm2).unwrap();
        a.vpsignd(xmm12, xmm1, xmmword_ptr(scratch + 16)).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // Src magnitudes + ctrl covering negative / zero / positive lanes across widths.
        let src: u128 = 0x8000_0001_7fff_ffff_1234_5678_9abc_def0;
        let ctrl: u128 = 0x8000_00ff_ff00_0080_007f_0000_ff80_0001;
        scratch_page[0..16].copy_from_slice(&src.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&ctrl.to_le_bytes());
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
        let native = run_native(&input).expect("host runs psign*/vpsign*");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on psign:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: SSE4.1 `insertps` (lane insert + zero mask), validated BIT-EXACT against the
    /// real CPU. Covers a source-lane select + zeroing, a no-zero insert, an all-zeroing imm,
    /// and the m32 memory form. SSE4.1 is present on all modern x86.
    #[test]
    fn native_insertps_matches_interp() {
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch + 16)).unwrap();
        a.movdqa(xmm2, xmm0).unwrap();
        a.insertps(xmm0, xmm1, 0x4E).unwrap(); // src lane1 → dst lane0, zero lanes 1-3
        a.insertps(xmm2, xmm1, 0xA0).unwrap(); // src lane2 → dst lane2, no zeroing
        a.movdqa(xmm3, xmm0).unwrap();
        a.insertps(xmm3, xmm1, 0x0F).unwrap(); // insert then zero ALL dwords
        a.insertps(xmm4, dword_ptr(scratch + 16), 0x20).unwrap(); // m32 → dst lane2
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        let d: u128 = 0x4048_0000_4040_0000_4000_0000_3f80_0000; // 1.0,2.0,3.0,3.125 f32
        let s: u128 = 0x42c8_0000_4296_0000_4248_0000_41a0_0000; // 20,50,75,100 f32
        scratch_page[0..16].copy_from_slice(&d.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&s.to_le_bytes());
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
        let native = run_native(&input).expect("host runs insertps");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on insertps:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-255: AVX `vinsertps` (VEX.128 3-operand), validated BIT-EXACT against the real
    /// CPU — the ground truth for the distinct merge base (`vvvv`), the imm8 src-lane/dst-lane
    /// selects + zmask, AND the VEX.128 upper-lane zeroing (ymm_hi is captured, so a missing
    /// `VZeroUpper` would diverge). Includes the exact Celeste wall shape
    /// `vinsertps xmm2, xmm0, xmm1, 0x10`, the wild `dst == src2` alias, and the m32 form.
    /// Self-skips without AVX.
    #[test]
    fn native_vinsertps_matches_interp() {
        if host_xsave_offsets().0 == 0 {
            return; // vinsertps (VEX) + ymm_hi capture need AVX
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch + 16)).unwrap();
        // Exact Celeste wall bytes c4 e3 79 21 d1 10: src lane0 → dst lane1, no zeroing.
        a.vinsertps(xmm2, xmm0, xmm1, 0x10i32).unwrap();
        a.vinsertps(xmm3, xmm0, xmm1, 0xAA).unwrap(); // src lane2 → dst lane2, zero 1&3
        a.vinsertps(xmm4, xmm0, xmm1, 0x3F).unwrap(); // src lane0 → dst lane3, zero ALL
        a.vinsertps(xmm0, xmm1, xmm0, 0x10).unwrap(); // wild: dst aliases src2 (op2)
        a.vinsertps(xmm5, xmm1, dword_ptr(scratch), 0x18).unwrap(); // m32 → dst lane1, zero 3
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();
        // The exact byte encoding (c4 e3 79 21 d1 10) is asserted by the differential test
        // `vinsertps_celeste_wild_bytes`; here we validate the runtime result vs the real CPU.

        let mut scratch_page = vec![0u8; 0x1000];
        let d: u128 = 0x4048_0000_4040_0000_4000_0000_3f80_0000; // 1.0,2.0,3.0,3.125 f32
        let s: u128 = 0x42c8_0000_4296_0000_4248_0000_41a0_0000; // 20,50,75,100 f32
        scratch_page[0..16].copy_from_slice(&d.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&s.to_le_bytes());
        // Dirty the upper halves of the destinations so the VEX.128 zeroing is observable.
        let mut init = CpuSnapshot::default();
        for r in [0usize, 2, 3, 4, 5] {
            init.ymm_hi[r] = u128::MAX;
        }
        let input = VectorInput {
            cpu_init: init,
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
        let native = run_native(&input).expect("AVX host runs vinsertps");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vinsertps:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-259: AVX1 `vmaskmovps`/`vmaskmovpd` — vector-mask conditional load/store validated
    /// against the real CPU. Covers ps+pd, xmm+ymm, a mask with mixed set/clear per-element
    /// sign bits, and — critically — a ymm store at the end of the mapped page whose masked-off
    /// high lanes point past it: hardware suppresses the access (no fault), so run_native only
    /// succeeds if x86jit agrees. Masks/data are seeded in scratch and loaded in-snippet; dest
    /// uppers are dirtied so VEX.128 zeroing / VEX.256 fill are observable. Self-skips w/o AVX.
    #[test]
    fn native_vmaskmov_matches_interp() {
        if host_xsave_offsets().0 == 0 {
            return; // AVX (VEX vmaskmov + ymm_hi capture) required
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(rax, scratch).unwrap();
        a.vmovdqu(ymm0, ymmword_ptr(rax)).unwrap(); // ps data
        a.vmovdqu(ymm1, ymmword_ptr(rax + 32)).unwrap(); // pd data
        a.vmovdqu(ymm2, ymmword_ptr(rax + 64)).unwrap(); // ps mask (mixed sign bits)
        a.vmovdqu(ymm4, ymmword_ptr(rax + 96)).unwrap(); // pd mask
        a.vmovdqu(ymm5, ymmword_ptr(rax + 128)).unwrap(); // sentinel
                                                          // Pre-seed store targets with the sentinel so masked-off lanes stay observable.
        a.vmovdqu(ymmword_ptr(rax + 160), ymm5).unwrap();
        a.vmovdqu(ymmword_ptr(rax + 192), ymm5).unwrap();
        // Masked stores + read back.
        a.vmaskmovps(ymmword_ptr(rax + 160), ymm2, ymm0).unwrap();
        a.vmovdqu(ymm6, ymmword_ptr(rax + 160)).unwrap();
        a.vmaskmovpd(ymmword_ptr(rax + 192), ymm4, ymm1).unwrap();
        a.vmovdqu(ymm8, ymmword_ptr(rax + 192)).unwrap();
        // Masked loads (masked-off lanes -> 0).
        a.vmaskmovps(ymm7, ymm2, ymmword_ptr(rax)).unwrap();
        a.vmaskmovpd(ymm9, ymm4, ymmword_ptr(rax + 32)).unwrap();
        // xmm forms.
        a.vmaskmovps(xmm10, xmm2, xmmword_ptr(rax)).unwrap();
        a.vmaskmovpd(xmm11, xmm4, xmmword_ptr(rax + 32)).unwrap();
        // Fault suppression: store at end of the mapped page; high lanes (past the page)
        // are masked off and must NOT fault. ymm12 mask has lanes 0..3 set, 4..7 clear.
        a.vmovdqu(ymm12, ymmword_ptr(rax + 224)).unwrap();
        a.lea(rcx, qword_ptr(rax + 0xFF0)).unwrap();
        a.vmaskmovps(ymmword_ptr(rcx), ymm12, ymm0).unwrap();
        a.vmovdqu(xmm13, xmmword_ptr(rcx)).unwrap(); // read back the in-page 16 bytes
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        let put =
            |p: &mut [u8], off: usize, v: u128| p[off..off + 16].copy_from_slice(&v.to_le_bytes());
        // ps data (8x f32), pd data (4x f64).
        put(&mut scratch_page, 0, 0x40800000_40400000_40000000_3f800000); // 4,3,2,1
        put(&mut scratch_page, 16, 0x41000000_40e00000_40c00000_40a00000); // 8,7,6,5
        put(&mut scratch_page, 32, 0x4010000000000000_3ff0000000000000); // 4.0,1.0
        put(&mut scratch_page, 48, 0x4022000000000000_4008000000000000); // 9.0,3.0
                                                                         // ps mask: per-32-bit lane MSB, mixed set/clear across all 8 lanes.
        put(&mut scratch_page, 64, 0x80000000_00000000_ffffffff_00000001);
        put(&mut scratch_page, 80, 0x00000000_80000000_7fffffff_80000000);
        // pd mask: per-64-bit lane MSB.
        put(&mut scratch_page, 96, 0x8000000000000000_0000000000000000);
        put(&mut scratch_page, 112, 0x0000000000000000_ffffffffffffffff);
        // Sentinel.
        put(
            &mut scratch_page,
            128,
            0xdeadbeef_deadbeef_deadbeef_deadbeef,
        );
        put(
            &mut scratch_page,
            144,
            0xcafef00d_cafef00d_cafef00d_cafef00d,
        );
        // Past-page mask: lanes 0..3 (low 16 bytes, in-page) set; lanes 4..7 (past page) clear.
        put(
            &mut scratch_page,
            224,
            0x80000000_80000000_80000000_80000000,
        );
        put(
            &mut scratch_page,
            240,
            0x00000000_00000000_00000000_00000000,
        );

        let mut init = CpuSnapshot::default();
        for r in [6usize, 7, 8, 9, 10, 11, 12, 13] {
            init.ymm_hi[r] = u128::MAX; // observe VEX.128 zeroing / VEX.256 fill
        }
        let input = VectorInput {
            cpu_init: init,
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
        let native = run_native(&input)
            .expect("AVX host runs vmaskmov (no fault on masked-off past-page lanes)");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vmaskmov:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: SSE4.1 `dpps` single-precision dot product, validated BIT-EXACT against the
    /// real CPU — the ground truth for the horizontal FP sum order, product mask, broadcast
    /// mask, and NaN propagation. A NaN lane is seeded so NaN handling is checked. Register
    /// and m128 memory forms. SSE4.1 is present on all modern x86.
    #[test]
    fn native_dpps_matches_interp() {
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch + 16)).unwrap();
        a.movdqa(xmm2, xmm0).unwrap();
        a.dpps(xmm0, xmm1, 0x71).unwrap(); // products 0,1,2 → dword 0
        a.dpps(xmm2, xmm1, 0xF5).unwrap(); // all products (NaN lane) → dwords 0,2
        a.movdqa(xmm3, xmm0).unwrap();
        a.dpps(xmm3, xmmword_ptr(scratch + 16), 0x31).unwrap(); // mem form
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // xmm0: 1.0, NaN, 3.0, 4.0 ; xmm1: 0.5, 0.25, 2.0, 100.0 (f32 bit patterns).
        let x0: u128 = 0x4080_0000_4040_0000_7fc0_0000_3f80_0000;
        let x1: u128 = 0x42c8_0000_4000_0000_3e80_0000_3f00_0000;
        scratch_page[0..16].copy_from_slice(&x0.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&x1.to_le_bytes());
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
        let native = run_native(&input).expect("host runs dpps");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on dpps:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-256: the VEX float cluster — `vblendvps/pd` + `vpblendvb` with an m128 src2 (the
    /// exact Celeste wall), the imm8 static blends `blendps/pd` + `vblendps/pd`, and the dot
    /// products `dppd` + `vdpps/vdppd` — all validated BIT-EXACT against the real AVX CPU, the
    /// ground truth for the variable/static blend selects, the horizontal FP sum, and the
    /// VEX.128 upper-lane zeroing (ymm_hi is captured, so a missing zero would diverge).
    /// Includes the Celeste-shaped `vblendvps xmm, xmm, [mem], xmm`. Self-skips without AVX.
    #[test]
    fn native_vex_float_cluster_matches_interp() {
        if host_xsave_offsets().0 == 0 {
            return; // VEX ops + ymm_hi capture need AVX
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch + 16)).unwrap();
        a.movdqu(xmm2, xmmword_ptr(scratch + 32)).unwrap(); // blend-control mask
                                                            // --- variable blend, m128 src2 (the Celeste wall shape) ---
        a.vblendvps(xmm3, xmm0, xmmword_ptr(scratch + 16), xmm2)
            .unwrap();
        a.vblendvpd(xmm4, xmm0, xmmword_ptr(scratch + 16), xmm2)
            .unwrap();
        a.vpblendvb(xmm5, xmm0, xmmword_ptr(scratch + 16), xmm2)
            .unwrap();
        a.vblendvps(xmm6, xmm0, xmm1, xmm2).unwrap(); // register form too
                                                      // --- imm8 static blends (SSE + VEX) ---
        a.movdqa(xmm7, xmm0).unwrap();
        a.blendps(xmm7, xmm1, 0b1010).unwrap(); // dwords 1,3 from src2
        a.movdqa(xmm8, xmm0).unwrap();
        a.blendpd(xmm8, xmm1, 0b10).unwrap(); // qword 1 from src2
        a.blendps(xmm9, xmmword_ptr(scratch + 16), 0b0101).unwrap(); // m128 (xmm9 = dst=src1)
        a.vblendps(xmm10, xmm0, xmm1, 0b0110).unwrap(); // VEX 3-operand
        a.vblendpd(xmm11, xmm0, xmm1, 0b01).unwrap();
        a.vblendps(xmm12, xmm0, xmmword_ptr(scratch + 16), 0b1001)
            .unwrap(); // VEX m128
                       // --- dot products ---
        a.movdqa(xmm13, xmm0).unwrap();
        a.dppd(xmm13, xmm1, 0x31).unwrap(); // both products → qword 0
        a.vdpps(xmm14, xmm0, xmm1, 0x71).unwrap(); // VEX single-precision dot
        a.vdppd(xmm15, xmm0, xmm1, 0x33).unwrap(); // VEX double-precision dot
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();
        // xmm9 dst==src1 for the SSE m128 blend; seed it below.

        let mut scratch_page = vec![0u8; 0x1000];
        // xmm0: f32 1,2,3,4 / f64 as bits; xmm1: f32 10,20,30,40; mask: alternating MSBs.
        let x0: u128 = 0x4080_0000_4040_0000_4000_0000_3f80_0000;
        let x1: u128 = 0x4220_0000_41f0_0000_41a0_0000_4120_0000;
        let mask: u128 = 0x8000_0000_0000_0000_ffff_ffff_ffff_ffff;
        scratch_page[0..16].copy_from_slice(&x0.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&x1.to_le_bytes());
        scratch_page[32..48].copy_from_slice(&mask.to_le_bytes());
        // Dirty the upper halves of the VEX destinations so their VEX.128 zeroing is observable.
        let mut init = CpuSnapshot::default();
        for r in [3usize, 4, 5, 6, 9, 10, 11, 12, 14, 15] {
            init.ymm_hi[r] = u128::MAX;
        }
        // xmm9 is dst==src1 for the SSE m128 blendps — give it a known merge base.
        init.xmm[9] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
        let input = VectorInput {
            cpu_init: init,
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
        let native = run_native(&input).expect("AVX host runs the VEX float cluster");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on the VEX float cluster:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-257: the exact-IEEE VEX float-op sweep — `vsqrtps`/`vsqrtpd` (packed sqrt),
    /// `vshufps`/`vshufpd` (3-operand shuffle), and `vunpck{l,h}p{s,d}` (float unpacks)
    /// validated BIT-EXACT against the real host AVX CPU. These ops are exact IEEE (unlike
    /// rcp/rsqrt), so a bit-exact oracle applies. Exercises the distinct merge base (vvvv),
    /// `dst == src2` aliasing (the shuffle/unpack wilds), and VEX.128 upper-lane zeroing
    /// (every dst's ymm_hi is dirtied, so a missing `VZeroUpper` diverges). Self-skips
    /// without AVX.
    #[test]
    fn native_vex_float_sweep_matches_interp() {
        if host_xsave_offsets().0 == 0 {
            return; // VEX ops + ymm_hi capture need AVX
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch + 16)).unwrap();
        // --- packed sqrt (register + m128) ---
        a.vsqrtps(xmm2, xmm0).unwrap();
        a.vsqrtpd(xmm3, xmm1).unwrap();
        a.vsqrtps(xmm4, xmmword_ptr(scratch)).unwrap(); // m128 source
                                                        // --- 3-operand shuffles (register, m128, dst==src2 wild) ---
        a.vshufps(xmm5, xmm0, xmm1, 0x1Bi32).unwrap();
        a.vshufpd(xmm6, xmm0, xmm1, 0x01i32).unwrap();
        a.vshufps(xmm7, xmm0, xmmword_ptr(scratch + 16), 0x4Ei32)
            .unwrap(); // m128 src2
        a.vshufps(xmm8, xmm1, xmm8, 0xB1i32).unwrap(); // wild: dst == src2
                                                       // --- float unpacks (register, m128, dst==src2 wild) ---
        a.vunpcklps(xmm9, xmm0, xmm1).unwrap();
        a.vunpckhps(xmm10, xmm0, xmm1).unwrap();
        a.vunpcklpd(xmm11, xmm0, xmm1).unwrap();
        a.vunpckhpd(xmm12, xmm0, xmm1).unwrap();
        a.vunpckhps(xmm13, xmm0, xmmword_ptr(scratch + 16)).unwrap(); // m128 src2
        a.vunpcklps(xmm14, xmm1, xmm14).unwrap(); // wild: dst == src2
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // xmm0: f32 4,9,16,25 → sqrt 2,3,4,5. xmm1: f64 4.0, 9.0 → sqrt 2, 3 (also read as f32
        // dword lanes by the shuffles/unpacks — that's fine, the oracle is the ground truth).
        let x0: u128 = 0x41c8_0000_4180_0000_4110_0000_4080_0000;
        let x1: u128 = 0x4022_0000_0000_0000_4010_0000_0000_0000;
        scratch_page[0..16].copy_from_slice(&x0.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&x1.to_le_bytes());
        let mut init = CpuSnapshot::default();
        for r in [2usize, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14] {
            init.ymm_hi[r] = u128::MAX; // observe VEX.128 upper-zeroing
        }
        // Seed the wild `dst == src2` merge bases (op1 of those instructions is xmm1).
        init.xmm[8] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
        init.xmm[14] = 0x5555_5555_6666_6666_7777_7777_8888_8888;
        let input = VectorInput {
            cpu_init: init,
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
        let native = run_native(&input).expect("AVX host runs the VEX float sweep");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on the VEX float sweep:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-258: the 256-bit (YMM) VEX float sweep — `vcvt{dq2ps,ps2dq,tps2dq}`, packed
    /// `vadd/sub/mul/div/min/max{ps,pd}`, `vsqrt{ps,pd}`, `vshuf{ps,pd}`, and
    /// `vunpck{l,h}p{s,d}` on ymm — validated BIT-EXACT against the real host AVX CPU. All
    /// are exact IEEE (or exact integer convert), so a bit-exact oracle applies. Exercises
    /// register + 32-byte memory src2, a `dst == src2` alias, `vshufpd`'s per-128-half imm,
    /// and the VEX.256 full-register write (both 128-bit halves observed via `ymm_hi`).
    /// Concrete Celeste blocker: `vcvtdq2ps ymm0, ymm0` (c5 fc 5b c0). Self-skips without AVX.
    #[test]
    fn native_vex_ymm_float_sweep_matches_interp() {
        if host_xsave_offsets().0 == 0 {
            return; // VEX.256 ops + ymm_hi capture need AVX
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(rax, scratch).unwrap();
        a.vmovups(ymm0, ymmword_ptr(rax)).unwrap();
        a.vmovups(ymm1, ymmword_ptr(rax + 32)).unwrap();
        // --- lane-preserving converts (register + m256) ---
        a.vcvtdq2ps(ymm2, ymm0).unwrap();
        a.vcvtps2dq(ymm3, ymm1).unwrap();
        a.vcvttps2dq(ymm4, ymm1).unwrap();
        a.vcvtdq2ps(ymm5, ymmword_ptr(rax)).unwrap();
        // --- packed arithmetic (register + m256 + dst==src2 wild) ---
        a.vaddps(ymm6, ymm0, ymm1).unwrap();
        a.vsubpd(ymm7, ymm0, ymm1).unwrap();
        a.vmulps(ymm8, ymm0, ymm1).unwrap();
        a.vdivpd(ymm9, ymm0, ymm1).unwrap();
        a.vminps(ymm10, ymm0, ymm1).unwrap();
        a.vmaxpd(ymm11, ymm0, ymm1).unwrap();
        a.vaddps(ymm12, ymm0, ymmword_ptr(rax + 32)).unwrap(); // m256 src2
        a.vaddps(ymm13, ymm1, ymm13).unwrap(); // wild: dst == src2
                                               // --- packed sqrt (register + m256) ---
        a.vsqrtps(ymm14, ymm0).unwrap();
        a.vsqrtpd(ymm15, ymm1).unwrap();
        // --- shuffles + unpacks (register + m256 + dst==src2 wild), reusing low dests ---
        a.vshufps(ymm2, ymm0, ymm1, 0x1Bi32).unwrap();
        a.vshufpd(ymm3, ymm0, ymm1, 0x09i32).unwrap(); // per-half imm differs
        a.vunpcklps(ymm4, ymm0, ymm1).unwrap();
        a.vunpckhps(ymm5, ymm0, ymm1).unwrap();
        a.vunpcklpd(ymm6, ymm0, ymm1).unwrap();
        a.vunpckhpd(ymm7, ymm0, ymm1).unwrap();
        a.vshufps(ymm8, ymm0, ymmword_ptr(rax + 32), 0x4Ei32)
            .unwrap(); // m256 src2
        a.vunpcklps(ymm9, ymm1, ymm9).unwrap(); // wild: dst == src2
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // ymm0: low int32/f32 4,3,2,1 ; high int32/f32 8,7,6,-5 (the same bits feed the
        // int-consuming converts and the float ops; the host CPU is ground truth for both).
        let y0_lo: u128 = 0x40800000_40400000_40000000_3f800000;
        let y0_hi: u128 = 0xc1000000_40e00000_40c00000_40a00000;
        // ymm1: low f32 2,2,2,2 ; high f64-ish bits reused as f32 lanes by ps ops (fine).
        let y1_lo: u128 = 0x40000000_40000000_40000000_40000000;
        let y1_hi: u128 = 0x41000000_41100000_40800000_40800000;
        scratch_page[0..16].copy_from_slice(&y0_lo.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&y0_hi.to_le_bytes());
        scratch_page[32..48].copy_from_slice(&y1_lo.to_le_bytes());
        scratch_page[48..64].copy_from_slice(&y1_hi.to_le_bytes());
        let mut init = CpuSnapshot::default();
        for r in 2..=15usize {
            init.ymm_hi[r] = u128::MAX; // observe the VEX.256 full-register write
        }
        // Seed the wild `dst == src2` merge bases (op1 of those instructions is ymm1).
        init.xmm[13] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
        init.ymm_hi[13] = 0x5555_5555_6666_6666_7777_7777_8888_8888;
        init.xmm[9] = 0x9999_9999_AAAA_AAAA_BBBB_BBBB_CCCC_CCCC;
        init.ymm_hi[9] = 0xDDDD_DDDD_EEEE_EEEE_FFFF_FFFF_0000_0000;
        let input = VectorInput {
            cpu_init: init,
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
        let native = run_native(&input).expect("AVX host runs the VEX.256 float sweep");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on the VEX.256 float sweep:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-257: `vrsqrtss`/`vrcpss` (scalar low lane) + `vrsqrtps`/`vrcpps` (all 4 lanes) run
    /// through the interpreter and checked against the TRUE math (`1.0/x`, `1.0/sqrt(x)`), NOT
    /// the host CPU — hardware returns a ~12-bit estimate that would not match our exact-IEEE
    /// output. We implement the exact reciprocal (see `FloatUnOp` docs), which trivially lies
    /// within the Intel SDM's guaranteed relative-error bound `1.5*2^-12 (~3.66e-4)`. The
    /// assertion checks our interp output is within that bound of the exact value over a range
    /// of positive inputs (and exact for `x == 1.0`). No AVX host required.
    #[test]
    fn native_vex_rcp_rsqrt_within_tolerance() {
        // Intel SDM guaranteed relative-error bound for the rcp/rsqrt estimate: 1.5 * 2^-12.
        let bound: f32 = 1.5 * 2.0f32.powi(-12);
        assert!((bound - 3.6621094e-4).abs() < 1e-9, "SDM bound sanity");

        let inputs: [f32; 8] = [0.5, 1.0, 2.0, 3.0, 7.0, 100.0, 0.001, 1234.5];
        let mut max_rcp_err: f32 = 0.0;
        let mut max_rsqrt_err: f32 = 0.0;

        let f32x4 = |a: f32, b: f32, c: f32, d: f32| -> u128 {
            (a.to_bits() as u128)
                | ((b.to_bits() as u128) << 32)
                | ((c.to_bits() as u128) << 64)
                | ((d.to_bits() as u128) << 96)
        };
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;

        // Drive four lanes at a time through vrsqrtps + vrcpps, and the low lane through the
        // scalar vrsqrtss/vrcpss, all on the interpreter.
        for chunk in inputs.chunks(4) {
            let mut lanes = [1.0f32; 4];
            for (i, &v) in chunk.iter().enumerate() {
                lanes[i] = v;
            }
            let mut a = CodeAssembler::new(64).unwrap();
            a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap();
            a.vrsqrtps(xmm1, xmm0).unwrap();
            a.vrcpps(xmm2, xmm0).unwrap();
            a.vrsqrtss(xmm3, xmm0, xmm0).unwrap(); // scalar low lane
            a.vrcpss(xmm4, xmm0, xmm0).unwrap();
            a.hlt().unwrap();
            let bytes = a.assemble(code).unwrap();

            let mut scratch_page = vec![0u8; 0x1000];
            let packed = f32x4(lanes[0], lanes[1], lanes[2], lanes[3]);
            scratch_page[0..16].copy_from_slice(&packed.to_le_bytes());
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
            let out =
                crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));

            let get_lane = |reg: u128, lane: usize| f32::from_bits((reg >> (lane * 32)) as u32);
            for (i, &x) in chunk.iter().enumerate() {
                let exact_rsqrt = 1.0f32 / x.sqrt();
                let exact_rcp = 1.0f32 / x;
                let got_rsqrt = get_lane(out.cpu.xmm[1], i);
                let got_rcp = get_lane(out.cpu.xmm[2], i);
                let rel = |got: f32, exact: f32| (got - exact).abs() / exact.abs();
                max_rsqrt_err = max_rsqrt_err.max(rel(got_rsqrt, exact_rsqrt));
                max_rcp_err = max_rcp_err.max(rel(got_rcp, exact_rcp));
                assert!(
                    rel(got_rsqrt, exact_rsqrt) <= bound,
                    "vrsqrtps lane {i} (x={x}): got {got_rsqrt}, exact {exact_rsqrt}, rel err > SDM bound"
                );
                assert!(
                    rel(got_rcp, exact_rcp) <= bound,
                    "vrcpps lane {i} (x={x}): got {got_rcp}, exact {exact_rcp}, rel err > SDM bound"
                );
                if i == 0 {
                    // Scalar low-lane forms: same exact-IEEE result as lane 0 of the packed.
                    let s_rsqrt = get_lane(out.cpu.xmm[3], 0);
                    let s_rcp = get_lane(out.cpu.xmm[4], 0);
                    assert!(
                        rel(s_rsqrt, exact_rsqrt) <= bound,
                        "vrsqrtss low lane out of bound"
                    );
                    assert!(
                        rel(s_rcp, exact_rcp) <= bound,
                        "vrcpss low lane out of bound"
                    );
                    // x == 1.0 must be exact (both are 1.0).
                    if x == 1.0 {
                        assert_eq!(s_rsqrt, 1.0, "vrsqrtss(1.0) == 1.0 exactly");
                        assert_eq!(s_rcp, 1.0, "vrcpss(1.0) == 1.0 exactly");
                    }
                }
            }
        }
        eprintln!(
            "task-257 rcp/rsqrt max rel-error: rsqrt={max_rsqrt_err:.3e}, rcp={max_rcp_err:.3e} (SDM bound {bound:.3e})"
        );
    }

    /// task-195: SSE4.2 `pcmpistrm`/`pcmpestrm` (mask → XMM0), validated BIT-EXACT against the
    /// real CPU — ground truth for the aggregation, the byte-mask vs bit-mask expansion
    /// (imm[6]), and the CF/ZF/SF/OF flags. Register (byte + bit mask) and the explicit-length
    /// memory form. SSE4.2 is present on all modern x86.
    #[test]
    fn native_pcmpistrm_matches_interp() {
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch + 16)).unwrap();
        a.pcmpistrm(xmm0, xmm1, 0x4C).unwrap(); // equal-ordered, byte mask
        a.movdqa(xmm2, xmm0).unwrap(); // preserve the mask before it's overwritten
        a.pcmpistrm(xmm0, xmm1, 0x18).unwrap(); // equal-each, bit mask
        a.movdqa(xmm3, xmm0).unwrap();
        a.mov(eax, 6).unwrap();
        a.mov(edx, 8).unwrap();
        a.pcmpestrm(xmm0, xmmword_ptr(scratch + 16), 0x0C).unwrap(); // explicit-length, mem
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        let s1: u128 = 0x00_00_6F_6C_6C_65_48_64_6C_72_6F_77_20_6F_6C_6C;
        let s2: u128 = 0x00_00_00_00_00_00_00_00_6C_72_6F_77_20_6F_6C_6C;
        scratch_page[0..16].copy_from_slice(&s1.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&s2.to_le_bytes());
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
        let native = run_native(&input).expect("host runs pcmpistrm/pcmpestrm");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on pcmpistrm:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-210: GFNI `gf2p8mulb/gf2p8affineqb/gf2p8affineinvqb` (SSE) + VEX.128 `vgf2p8*`,
    /// validated BIT-EXACT against the real CPU (host has GFNI). This is the ground-truth
    /// check for the GF(2^8) multiply and the affine matrix bit/row ordering + imm8 XOR.
    /// Input + matrix staged in scratch. Self-skips without GFNI.
    #[test]
    fn native_gfni_matches_interp() {
        if !std::is_x86_feature_detected!("gfni") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch)).unwrap(); // input byte vector
        a.movdqu(xmm2, xmmword_ptr(scratch + 16)).unwrap(); // multiplier / affine matrix
                                                            // SSE in-place: copy input into each dst, then apply the op.
        a.movdqa(xmm0, xmm1).unwrap();
        a.gf2p8mulb(xmm0, xmm2).unwrap();
        a.movdqa(xmm3, xmm1).unwrap();
        a.gf2p8affineqb(xmm3, xmm2, 0x5au32).unwrap();
        a.movdqa(xmm4, xmm1).unwrap();
        a.gf2p8affineinvqb(xmm4, xmm2, 0xa5u32).unwrap();
        // SSE memory second-source form.
        a.movdqa(xmm5, xmm1).unwrap();
        a.gf2p8affineqb(xmm5, xmmword_ptr(scratch + 16), 0x11u32)
            .unwrap();
        // VEX.128 3-operand (dst distinct, register + memory second source).
        a.vgf2p8mulb(xmm9, xmm1, xmm2).unwrap();
        a.vgf2p8affineqb(xmm10, xmm1, xmm2, 0x3cu32).unwrap();
        a.vgf2p8affineinvqb(xmm11, xmm1, xmm2, 0xc3u32).unwrap();
        a.vgf2p8mulb(xmm12, xmm1, xmmword_ptr(scratch + 16))
            .unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // Non-trivial input + matrix so every byte lane and matrix row is exercised.
        let x: u128 = 0x0f1e_2d3c_4b5a_6978_8796_a5b4_c3d2_e1f0;
        let mat: u128 = 0x1032_5476_98ba_dcfe_efcd_ab89_6745_2301;
        scratch_page[0..16].copy_from_slice(&x.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&mat.to_le_bytes());
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
        let native = run_native(&input).expect("GFNI host runs gf2p8*/vgf2p8*");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on GFNI:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-211: PCLMULQDQ `pclmulqdq` (SSE) + VEX.128 `vpclmulqdq`, validated BIT-EXACT
    /// against the real CPU (host has PCLMULQDQ). Ground-truth check for the carry-less
    /// GF(2)[x] multiply + the imm8 half-selection (all four `0x00/0x01/0x10/0x11`).
    /// Operands staged in scratch. Self-skips without PCLMULQDQ.
    #[test]
    fn native_pclmul_matches_interp() {
        if !std::is_x86_feature_detected!("pclmulqdq") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm1, xmmword_ptr(scratch)).unwrap(); // op1
        a.movdqu(xmm2, xmmword_ptr(scratch + 16)).unwrap(); // op2
                                                            // SSE in-place: all four imm8 half-selections.
        a.movdqa(xmm0, xmm1).unwrap();
        a.pclmulqdq(xmm0, xmm2, 0x00).unwrap(); // lo·lo
        a.movdqa(xmm3, xmm1).unwrap();
        a.pclmulqdq(xmm3, xmm2, 0x01).unwrap(); // hi·lo
        a.movdqa(xmm4, xmm1).unwrap();
        a.pclmulqdq(xmm4, xmm2, 0x10).unwrap(); // lo·hi
        a.movdqa(xmm5, xmm1).unwrap();
        a.pclmulqdq(xmm5, xmm2, 0x11).unwrap(); // hi·hi
                                                // SSE memory second-source form.
        a.movdqa(xmm6, xmm1).unwrap();
        a.pclmulqdq(xmm6, xmmword_ptr(scratch + 16), 0x10).unwrap();
        // VEX.128 3-operand (dst distinct, register + memory second source).
        a.vpclmulqdq(xmm9, xmm1, xmm2, 0x00).unwrap();
        a.vpclmulqdq(xmm10, xmm1, xmm2, 0x11).unwrap();
        a.vpclmulqdq(xmm11, xmm1, xmmword_ptr(scratch + 16), 0x01)
            .unwrap();
        // VEX dst aliasing the second source must not clobber early (dst==src reg).
        a.vpclmulqdq(xmm2, xmm1, xmm2, 0x10).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // Non-trivial operands so both halves and many bit positions are exercised.
        let op1: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
        let op2: u128 = 0xdead_beef_cafe_babe_0bad_f00d_feed_face;
        scratch_page[0..16].copy_from_slice(&op1.to_le_bytes());
        scratch_page[16..32].copy_from_slice(&op2.to_le_bytes());
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
        let native = run_native(&input).expect("PCLMULQDQ host runs pclmulqdq/vpclmulqdq");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on PCLMULQDQ:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: dword packed min/max `vpmin/max{u,s}d` (VEX + EVEX), validated against
    /// the real CPU — the native oracle previously caught these being undispatched. Wide
    /// inputs staged in scratch. Self-skips without AVX-512F.
    #[test]
    fn native_dword_minmax_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu(xmm1, xmmword_ptr(scratch)).unwrap();
        a.vmovdqu(xmm2, xmmword_ptr(scratch + 16)).unwrap();
        a.vpminud(xmm0, xmm1, xmm2).unwrap();
        a.vpmaxsd(xmm3, xmm1, xmm2).unwrap();
        a.vmovdqu64(zmm4, zmmword_ptr(scratch)).unwrap();
        a.vmovdqu64(zmm5, zmmword_ptr(scratch + 64)).unwrap();
        a.vpminsd(zmm6, zmm4, zmm5).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(128).enumerate() {
            *b = (i as u8).wrapping_mul(61).wrapping_add(0x81);
        }
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
        let native = run_native(&input).expect("AVX-512F host runs dword min/max");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on dword min/max:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: cross-lane permutes `vpermq`/`vpermd` (single-source), `vpermi2d`, and
    /// memory-source `vpermt2d`, validated against the real CPU. Inputs staged in scratch.
    /// Self-skips without AVX-512F.
    #[test]
    fn native_permute_family_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm0, zmmword_ptr(scratch)).unwrap(); // data
        a.vmovdqu64(zmm3, zmmword_ptr(scratch + 64)).unwrap(); // index
        a.vmovdqu64(zmm1, zmmword_ptr(scratch + 128)).unwrap(); // table0/i2 table
        a.vpermq(zmm4, zmm3, zmm0).unwrap();
        a.vpermd(zmm5, zmm3, zmm0).unwrap();
        a.vpermi2d(zmm6, zmm1, zmm0).unwrap(); // idx = old zmm6 (zeroed) → picks lane 0s
        a.vpermt2d(zmm1, zmm3, zmmword_ptr(scratch)).unwrap(); // mem table1
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(192).enumerate() {
            *b = (i as u8).wrapping_mul(29).wrapping_add(1);
        }
        // index dwords/qwords: keep them small (masked to log2(n) bits anyway)
        for i in 0..16usize {
            scratch_page[64 + i * 4] = ((i * 7) & 0x0F) as u8;
        }
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
        let native = run_native(&input).expect("AVX-512F host runs the permute family");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on the permute family:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: VEX-128 `vinserti128` (mem), `vpblendw`, `vpackusdw`/`vpacksswb`, and
    /// scalar `vsqrtsd`, validated against the real CPU. Inputs staged in scratch.
    #[test]
    fn native_vinsert_blend_pack_sqrt_matches_interp() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu(ymm1, ymmword_ptr(scratch)).unwrap();
        a.vmovdqu(xmm2, xmmword_ptr(scratch + 32)).unwrap();
        a.vinserti128(ymm0, ymm1, xmmword_ptr(scratch + 32), 1)
            .unwrap();
        a.vpblendw(xmm3, xmm1, xmm2, 0x5A).unwrap();
        a.vpackusdw(xmm4, xmm1, xmm2).unwrap();
        a.vpacksswb(xmm5, xmm1, xmm2).unwrap();
        a.vsqrtsd(xmm6, xmm1, xmm2).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(48).enumerate() {
            *b = (i as u8).wrapping_mul(43).wrapping_add(7);
        }
        // a valid positive double at scratch+32 for the sqrt (xmm2 low qword)
        scratch_page[32..40].copy_from_slice(&(2.25f64).to_bits().to_le_bytes());
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
        let native = run_native(&input).expect("AVX2 host runs vinsert/blend/pack/sqrt");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vinsert/blend/pack/sqrt:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: opmask shift `kshift{l,r}{w,d,q}`, validated against the real CPU. Masks
    /// built in-snippet. Self-skips without AVX-512BW.
    #[test]
    fn native_kshift_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") {
            return;
        }
        let code = 0x21_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(eax, 0xF0F0u32 as i32).unwrap();
        a.kmovd(k1, eax).unwrap();
        a.kshiftld(k2, k1, 3).unwrap();
        a.kshiftrd(k3, k1, 5).unwrap();
        a.kshiftlw(k4, k1, 20).unwrap(); // ≥ width → cleared
        a.kshiftrq(k5, k1, 1).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();
        let input = VectorInput {
            cpu_init: CpuSnapshot::default(),
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };
        let native = run_native(&input).expect("AVX-512 host runs kshift");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on kshift:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: opmask bitwise logic `k{or,and,andn,xor,xnor}{b,d}` + `knot`, validated
    /// against the real CPU. Masks are built in-snippet (GPR → kmov), so no wide init is
    /// needed. Self-skips without AVX-512BW (the byte-width `korb`/`kandb` forms).
    #[test]
    fn native_opmask_logic_family_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") {
            return;
        }
        let code = 0x21_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(eax, 0xF0F0u32 as i32).unwrap();
        a.kmovd(k1, eax).unwrap();
        a.mov(eax, 0x3C5Au32 as i32).unwrap();
        a.kmovd(k2, eax).unwrap();
        a.kord(k3, k1, k2).unwrap();
        a.korb(k4, k1, k2).unwrap();
        a.kandd(k5, k1, k2).unwrap();
        a.kandnd(k6, k1, k2).unwrap();
        a.kxord(k7, k1, k2).unwrap();
        a.kxnord(k1, k1, k2).unwrap();
        a.knotd(k2, k2).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let input = VectorInput {
            cpu_init: CpuSnapshot::default(),
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };
        let native = run_native(&input).expect("AVX-512 host runs opmask logic");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on opmask logic:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.5: EVEX masked packed arithmetic `vpaddd`/`vpsubd`/`vpminud` under a
    /// write-mask, validated against the real CPU (128-bit → xmm init only). Self-skips
    /// without AVX-512VL.
    #[test]
    fn native_masked_packed_arith_matches_interp() {
        if !std::is_x86_feature_detected!("avx512vl") {
            return;
        }
        let code = 0x21_0000u64;
        let a_pat: u128 = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
        let b_pat: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
        let d_pat: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;

        let mut a = CodeAssembler::new(64).unwrap();
        a.mov(eax, 0b1010i32).unwrap();
        a.kmovw(k1, eax).unwrap();
        a.vpaddd(xmm0.k1(), xmm1, xmm2).unwrap(); // merge
        a.vpsubd(xmm3.k1().z(), xmm1, xmm2).unwrap(); // zero
        a.vpaddq(xmm4.k1(), xmm1, xmm2).unwrap(); // qword granularity
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[1] = a_pat;
        init.xmm[2] = b_pat;
        init.xmm[0] = d_pat;
        init.xmm[3] = d_pat;
        init.xmm[4] = d_pat;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };
        let native = run_native(&input).expect("AVX-512VL host runs masked packed arith");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on masked packed arith:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: the VEX.128 + scalar ops the coreutils corpus hits — `vpunpcklqdq`,
    /// `vpsrldq`, `vcvtsd2ss`, and EVEX `vrndscalesd` (M=0) — plus the narrowing move
    /// `vpmovdw` with a ZMM source staged in scratch. Validated against the real CPU.
    /// Self-skips without AVX-512BW.
    #[test]
    fn native_vex128_narrow_vrndscale_matches_interp() {
        if !std::is_x86_feature_detected!("avx512bw") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap(); // 64-byte source for vpmovdw
        a.vpmovdw(ymm5, zmm1).unwrap(); // 16 dwords → 16 words
        a.movdqu(xmm1, xmmword_ptr(scratch)).unwrap();
        a.movdqu(xmm2, xmmword_ptr(scratch + 16)).unwrap();
        a.vpunpcklqdq(xmm0, xmm1, xmm2).unwrap();
        a.vpsrldq(xmm3, xmm1, 5).unwrap();
        a.vcvtsd2ss(xmm4, xmm1, xmm2).unwrap();
        a.vrndscalesd(xmm6, xmm1, xmm2, 0x01).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(64).enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        // A valid double + a value to round in the low qword of the second chunk.
        scratch_page[16..24].copy_from_slice(&(13.7f64).to_bits().to_le_bytes());
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
        let native = run_native(&input).expect("AVX-512BW host runs VEX128 + narrow + vrndscale");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on VEX128/narrow/vrndscale:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-195: memory-source `pcmpistri xmm, [mem], imm` validated against the real CPU.
    /// The needle is staged in scratch; ECX gets the match index and the flags are set.
    #[test]
    fn native_pcmpistri_mem_src_matches_interp() {
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.movdqu(xmm0, xmmword_ptr(scratch)).unwrap(); // haystack "hello"
        a.pcmpistri(xmm0, xmmword_ptr(scratch + 16), 0x0C).unwrap(); // needle from mem
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..5].copy_from_slice(b"hello");
        scratch_page[16..18].copy_from_slice(b"ll");
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
        let native = run_native(&input).expect("host runs pcmpistri with a memory src");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on pcmpistri mem-src:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-193: capture the ZMM upper halves (bits 511:256) and an opmask from the real
    /// CPU. A snippet loads a 64-byte pattern into a ZMM register and sets a k register;
    /// the captured state must match the interpreter. Self-skips without AVX-512.
    #[test]
    fn native_captures_zmm_and_opmask() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let pattern: Vec<u8> = (0..64u8)
            .map(|b| b.wrapping_mul(3).wrapping_add(1))
            .collect();
        // Upper 256 bits (bytes 32..64) as two u128 halves.
        let zhi0 = u128::from_le_bytes(pattern[32..48].try_into().unwrap());
        let zhi1 = u128::from_le_bytes(pattern[48..64].try_into().unwrap());

        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm3, zmmword_ptr(scratch)).unwrap(); // full 512-bit load
        a.mov(eax, 0x1234i32).unwrap();
        a.kmovw(k2, eax).unwrap(); // k2 = 0x1234
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[..64].copy_from_slice(&pattern);
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

        let native = run_native(&input).expect("AVX-512 host runs vmovdqu64/kmovw");
        assert_eq!(
            native.cpu.zmm_hi[3],
            [zhi0, zhi1],
            "zmm3 bits 511:256 (real CPU)"
        );
        assert_eq!(native.cpu.kmask[2], 0x1234, "k2 (real CPU)");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on ZMM/opmask capture:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-168.5.4: SSE4.1 `roundps` (nearest-even) and `blendvps` validated against the
    /// real CPU. The round case includes `-0.5`, which must round to `-0.0` (signed zero)
    /// — the exact hardware behaviour the interpreter was corrected to match.
    #[test]
    fn native_sse41_round_blendv_matches_interp() {
        if !std::is_x86_feature_detected!("sse4.1") {
            return;
        }
        let code = 0x21_0000u64;
        let f32x4 = |a: f32, b: f32, c: f32, d: f32| {
            (a.to_bits() as u128)
                | ((b.to_bits() as u128) << 32)
                | ((c.to_bits() as u128) << 64)
                | ((d.to_bits() as u128) << 96)
        };

        let mut a = CodeAssembler::new(64).unwrap();
        a.roundps(xmm2, xmm1, 0).unwrap(); // nearest-even, incl. -0.5 -> -0.0
        a.blendvps(xmm3, xmm4).unwrap(); // blend by XMM0 lane MSBs
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[1] = f32x4(2.5, -2.5, 3.5, -0.5);
        init.xmm[0] = 0x8000_0000_0000_0000_8000_0000_0000_0000; // lanes 0,2 pick src
        init.xmm[3] = f32x4(1.0, 2.0, 3.0, 4.0);
        init.xmm[4] = f32x4(9.0, 9.0, 9.0, 9.0);
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("SSE4.1 host runs roundps/blendvps");
        // Lane 3 of the round result is -0.0 (0x8000_0000), not +0.0.
        assert_eq!(native.cpu.xmm[2] >> 96, 0x8000_0000, "roundps(-0.5) = -0.0");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on SSE4.1 round/blendv:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-202 regression: 3-operand VEX scalar float ops where op2 (the r/m source)
    /// aliases the destination register — `vaddsd xmm0, xmm1, xmm0` and the
    /// non-commutative `vsubsd xmm0, xmm1, xmm0`. This is exactly what CPython 3.14's
    /// `_PyLong_Frexp` Horner loop emits; a broken lift pre-copied op1 into dst and
    /// clobbered op2 before reading it, so `float(2**30)` came out 0.0 under --cpu v4.
    /// Both sums/differences are validated against the real CPU.
    #[test]
    fn native_vaddsd_dst_aliases_src2_matches_interp() {
        if !std::is_x86_feature_detected!("avx") {
            return;
        }
        let code = 0x21_0000u64;
        // xmm0 = 2^55, xmm1 = 0.0 (the frexp case: op2 is the big value, op1 is 0).
        // Correct: xmm0 = op1 + op2 = 0 + 2^55 = 2^55. The bug produced op1+op1 = 0.
        // xmm2 = 5.0, xmm3 = 3.0 for the sub check: xmm3 - xmm2 = -2 (order matters).
        let big = (2.0f64.powi(55)).to_bits() as u128;

        let mut a = CodeAssembler::new(64).unwrap();
        a.vaddsd(xmm0, xmm1, xmm0).unwrap(); // xmm0 = xmm1 + xmm0  (dst == src2)
        a.vsubsd(xmm2, xmm3, xmm2).unwrap(); // xmm2 = xmm3 - xmm2 = 3 - 5 = -2 (order!)
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[0] = big;
        init.xmm[1] = 0;
        init.xmm[2] = (5.0f64).to_bits() as u128;
        init.xmm[3] = (3.0f64).to_bits() as u128;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX host runs vaddsd/vsubsd");
        assert_eq!(native.cpu.xmm[0], big, "vaddsd(0, 2^55) = 2^55");
        assert_eq!(
            f64::from_bits(native.cpu.xmm[2] as u64),
            -2.0,
            "vsubsd(xmm3=3, xmm2=5) low lane = 3 - 5 = -2 (op2==dst, order preserved)"
        );
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on dst==src2 vaddsd/vsubsd:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-203: the AVX in-place ops of the 3-operand `op2==dst` aliasing family —
    /// `vpshufb`, `vpalignr`, `vsqrtsd`, `vmovsd` with the register op2 aliasing dst.
    /// Each lift now carries an explicit source so op2 isn't clobbered by a pre-copy.
    /// Validated against the real CPU (interp must match hardware exactly). The EVEX
    /// round sibling (`vrndscalesd`) is covered separately (needs AVX-512).
    #[test]
    fn native_vex_alias_family_matches_interp() {
        if !std::is_x86_feature_detected!("avx") {
            return;
        }
        let code = 0x21_0000u64;
        let data: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
        let ctrl: u128 = 0x8080_8080_0001_0203_0405_0607_0809_0a0b;
        let five = (5.0f64).to_bits() as u128;

        let mut a = CodeAssembler::new(64).unwrap();
        a.vpshufb(xmm0, xmm1, xmm0).unwrap(); // shuffle op1 by control == dst
        a.vpalignr(xmm2, xmm1, xmm2, 5).unwrap(); // concat op1:op2, op2 == dst
        a.vsqrtsd(xmm4, xmm1, xmm4).unwrap(); // sqrt(op2==dst), merge op1 upper
        a.db(&[0xc5, 0xf3, 0x10, 0xed]).unwrap(); // vmovsd xmm5,xmm1,xmm5 (no 3-op asm)
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[1] = data;
        init.xmm[0] = ctrl;
        for r in [4, 5] {
            init.xmm[r] = five;
        }
        init.xmm[2] = five;
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX host runs the alias-family snippet");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on the dst==src2 VEX family:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-203: the EVEX `vrndscalesd xmm3, xmm1, xmm3` round with op2 aliasing dst —
    /// the VPRound arm of the aliasing family. Needs an AVX-512 host for the native
    /// oracle to run the EVEX encoding.
    #[test]
    fn native_vrndscale_alias_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vrndscalesd(xmm3, xmm1, xmm3, 1).unwrap(); // round op2==dst toward -inf, merge op1
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut init = CpuSnapshot::default();
        init.xmm[1] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100; // op1 upper bytes
        init.xmm[3] = (5.7f64).to_bits() as u128; // op2 == dst, low lane rounded
        let input = VectorInput {
            cpu_init: init,
            mem_init: vec![MemChunk {
                addr: code,
                bytes,
                kind: MemKind::Ram,
            }],
            entry: code,
            run: RunSpec::UntilExit,
        };

        let native = run_native(&input).expect("AVX-512 host runs vrndscalesd");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vrndscalesd op2==dst:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-215: EVEX-512 packed shift-by-imm `vpsr{l,a}{d,q}`/`vpsl{l}{d,q}` at ZMM
    /// width, unmasked + merge/zeroing masked. Validated against the real CPU (the
    /// openssl-genrsa trap chain started here). Self-skips without AVX-512F.
    #[test]
    fn native_masked_shift_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap();
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap(); // merge base
        a.vpsrld(zmm3, zmm1, 0x1fu32).unwrap(); // the exact genrsa trap
        a.vpslld(zmm4, zmm1, 3u32).unwrap();
        a.vpsrad(zmm5, zmm1, 5u32).unwrap();
        a.vpsrlq(zmm6, zmm1, 17u32).unwrap();
        a.vpsllq(zmm7, zmm1, 40u32).unwrap();
        a.vpsraq(zmm8, zmm1, 63u32).unwrap();
        // Masked: merge (keep zmm2 lanes) + zeroing.
        a.mov(eax, 0x0000_cc33u32).unwrap();
        a.kmovd(k1, eax).unwrap();
        a.vmovdqa64(zmm9, zmm2).unwrap();
        a.vpsrld(zmm9.k1(), zmm1, 4u32).unwrap(); // merge
        a.vpslld(zmm10.k1().z(), zmm1, 6u32).unwrap(); // zeroing
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(128).enumerate() {
            *b = (i as u8).wrapping_mul(0x33).wrapping_add(0x81); // varied, sign bits set
        }
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
        let native = run_native(&input).expect("AVX-512F host runs vpsr/vpsl zmm");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on EVEX-512 packed shift:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-215: `pmuludq`/`vpmuludq` unsigned low-dword → 64-bit product across SSE,
    /// VEX.128, VEX.256 and EVEX.512, register and memory second source. Validated
    /// against the real CPU (openssl RSA prime derivation relies on it). Needs AVX-512F.
    #[test]
    fn native_vpmuludq_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm1, zmmword_ptr(scratch)).unwrap();
        a.vmovdqu64(zmm2, zmmword_ptr(scratch + 64)).unwrap();
        // SSE in-place.
        a.movdqa(xmm0, xmm1).unwrap();
        a.pmuludq(xmm0, xmm2).unwrap();
        // VEX.128 reg + mem.
        a.vpmuludq(xmm3, xmm1, xmm2).unwrap();
        a.vpmuludq(xmm4, xmm1, xmmword_ptr(scratch + 64)).unwrap();
        // VEX.256 reg + mem (the genrsa trap form).
        a.vpmuludq(ymm5, ymm1, ymm2).unwrap();
        a.vpmuludq(ymm6, ymm1, ymmword_ptr(scratch + 64)).unwrap();
        // EVEX.512 reg + mem (RSA-2048 montgomery multiply).
        a.vpmuludq(zmm7, zmm1, zmm2).unwrap();
        a.vpmuludq(zmm8, zmm1, zmmword_ptr(scratch + 64)).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // Values with high dwords set so masking-to-low-32 actually matters.
        for (i, b) in scratch_page.iter_mut().take(128).enumerate() {
            *b = (i as u8).wrapping_mul(0x57).wrapping_add(0x9a);
        }
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
        let native = run_native(&input).expect("AVX-512F host runs pmuludq/vpmuludq");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vpmuludq:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-215: memory-source single-table permute `vperm{q,d} v, idx, [mem]` (EVEX-512,
    /// the openssl-genrsa-1024 trap). Validated against the real CPU. Needs AVX-512F.
    #[test]
    fn native_vperm1_mem_matches_interp() {
        if !std::is_x86_feature_detected!("avx512f") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu64(zmm0, zmmword_ptr(scratch)).unwrap(); // index vectors
        a.vpermq(zmm1, zmm0, zmmword_ptr(scratch + 64)).unwrap(); // qword gather from mem
        a.vpermd(zmm2, zmm0, zmmword_ptr(scratch + 64)).unwrap(); // dword gather from mem
                                                                  // Masked merge + zeroing memory-source forms.
        a.mov(eax, 0x0000_a5c3u32).unwrap();
        a.kmovd(k1, eax).unwrap();
        a.vmovdqa64(zmm3, zmm0).unwrap();
        a.vpermq(zmm3.k1(), zmm0, zmmword_ptr(scratch + 64))
            .unwrap(); // merge
        a.vpermd(zmm4.k1().z(), zmm0, zmmword_ptr(scratch + 64))
            .unwrap(); // zeroing
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        // First 64 bytes = index lanes (need low bits varied 0..7 for qword, 0..15 dword);
        // next 64 = the table to gather from.
        for (i, b) in scratch_page.iter_mut().take(64).enumerate() {
            *b = (i as u8).wrapping_mul(0x2b).wrapping_add(i as u8);
        }
        for (i, b) in scratch_page.iter_mut().skip(64).take(64).enumerate() {
            *b = (i as u8).wrapping_mul(0x91).wrapping_add(0x13);
        }
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
        let native = run_native(&input).expect("AVX-512F host runs vpermq/vpermd mem-src");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vperm1 mem-src:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-215: the AVX2 256-bit op battery openssl's rsaz path leans on
    /// (vpaddq/vpsubq/vpsrlq/vpsllq/vpand/vpermq/vpshufd/vpbroadcastq/vpor/vpxor),
    /// fuzzed vs the REAL CPU over many random vectors. Guards the rsaz/bignum lifts.
    #[test]
    fn native_rsaz_avx2_battery_matches_interp() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu(ymm1, ymmword_ptr(scratch)).unwrap();
        a.vmovdqu(ymm2, ymmword_ptr(scratch + 32)).unwrap();
        a.vpaddq(ymm3, ymm1, ymm2).unwrap();
        a.vpsubq(ymm4, ymm1, ymm2).unwrap();
        a.vpsrlq(ymm5, ymm1, 29u32).unwrap();
        a.vpsllq(ymm6, ymm1, 29u32).unwrap();
        a.vpand(ymm7, ymm1, ymm2).unwrap();
        a.vpermq(ymm8, ymm1, 0x93).unwrap();
        a.vpshufd(ymm9, ymm1, 0x4e).unwrap();
        a.vpbroadcastq(ymm10, xmm1).unwrap();
        a.vpbroadcastq(ymm11, qword_ptr(scratch + 8)).unwrap();
        a.vpor(ymm12, ymm1, ymm2).unwrap();
        a.vpxor(ymm13, ymm1, ymm2).unwrap();
        a.vpshufb(ymm14, ymm1, ymm2).unwrap();
        a.vpaddd(ymm15, ymm1, ymm2).unwrap();
        a.vpsrld(ymm0, ymm1, 7u32).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut state = 0x1234_5678_9abc_def0u64;
        for iter in 0..48 {
            let mut scratch_page = vec![0u8; 0x1000];
            for b in scratch_page.iter_mut().take(64) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *b = (state & 0xff) as u8;
            }
            let input = VectorInput {
                cpu_init: CpuSnapshot::default(),
                mem_init: vec![
                    MemChunk {
                        addr: code,
                        bytes: bytes.clone(),
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
            let native = run_native(&input).expect("AVX2 host runs the battery");
            let interp =
                crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
            assert!(
                crate::compare::compare(&native, &interp, &[]).is_none(),
                "iter {iter}: interp diverges from real CPU on rsaz-avx2 battery:\n{:#?}",
                crate::compare::compare(&native, &interp, &[])
            );
        }
    }

    /// task-215: exhaustively check VEX.256 packed shift-by-imm at EVERY count (0..=64
    /// qword, 0..=32 dword) vs the real CPU — rsaz's 29-bit redundant form uses specific
    /// counts; an over-shift or off-by-one edge would only show at a particular count.
    #[test]
    fn native_avx2_shift_all_counts_match_interp() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(32).enumerate() {
            *b = (i as u8).wrapping_mul(0x93).wrapping_add(0x8f); // sign bits set for sra
        }
        for cnt in 0u32..=64 {
            let mut a = CodeAssembler::new(64).unwrap();
            a.vmovdqu(ymm1, ymmword_ptr(scratch)).unwrap();
            a.vpsrlq(ymm2, ymm1, cnt).unwrap();
            a.vpsllq(ymm3, ymm1, cnt).unwrap();
            if cnt <= 32 {
                a.vpsrld(ymm4, ymm1, cnt).unwrap();
                a.vpslld(ymm5, ymm1, cnt).unwrap();
                a.vpsrad(ymm6, ymm1, cnt).unwrap();
            }
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
                        bytes: scratch_page.clone(),
                        kind: MemKind::Ram,
                    },
                ],
                entry: code,
                run: RunSpec::UntilExit,
            };
            let native = run_native(&input).expect("AVX2 host runs packed shifts");
            let interp =
                crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
            assert!(
                crate::compare::compare(&native, &interp, &[]).is_none(),
                "count {cnt}: interp diverges from real CPU on AVX2 packed shift:\n{:#?}",
                crate::compare::compare(&native, &interp, &[])
            );
        }
    }

    /// task-215: `vpblendd` per-dword immediate blend, VEX.128 + VEX.256. Validated
    /// against the real CPU (openssl emits it in its RSA path). Needs AVX2.
    #[test]
    fn native_vpblendd_matches_interp() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        a.vmovdqu(ymm1, ymmword_ptr(scratch)).unwrap();
        a.vmovdqu(ymm2, ymmword_ptr(scratch + 32)).unwrap();
        a.vpblendd(xmm3, xmm1, xmm2, 0x3).unwrap();
        a.vpblendd(xmm4, xmm1, xmm2, 0xa).unwrap();
        a.vpblendd(ymm5, ymm1, ymm2, 0x3).unwrap(); // the genrsa trap form
        a.vpblendd(ymm6, ymm1, ymm2, 0x5a).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        let mut scratch_page = vec![0u8; 0x1000];
        for (i, b) in scratch_page.iter_mut().take(64).enumerate() {
            *b = if i < 32 { 0x11u8 } else { 0xee }; // distinct a vs b bytes
        }
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
        let native = run_native(&input).expect("AVX2 host runs vpblendd");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on vpblendd:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }

    /// task-260: the arithmetic-sensitive new VEX packed-int forms validated BIT-EXACT
    /// against the real CPU — the saturation edges of `vpaddsb/vpaddusw/vpsubsw`, the
    /// rounding of `vpavgb`, the rounded-high multiply of `vpmulhrsw`, and the signed-word
    /// saturation of `vpmaddubsw` (plus `vpmaddwd`). The interpreter is only the JIT's
    /// oracle, so hardware is the real ground truth for these. Operands are packed with
    /// 0x80/0x7f/0x8000/0x7fff edges so saturation/rounding boundaries are hit. Guarded on
    /// the host's XSAVE/YMM support (self-skips on hosts without it).
    #[test]
    fn native_packed_int_sweep_matches_interp() {
        if host_xsave_offsets().0 == 0 || !std::is_x86_feature_detected!("avx2") {
            return;
        }
        let code = 0x21_0000u64;
        let scratch = 0x22_0000u64;
        let mut a = CodeAssembler::new(64).unwrap();
        // ymm0/ymm1 = the two 32-byte operands loaded from scratch; [scratch+64] is the
        // 32-byte memory src2 (== ymm1's bytes) for the memory forms.
        a.vmovdqu(ymm0, ymmword_ptr(scratch)).unwrap();
        a.vmovdqu(ymm1, ymmword_ptr(scratch + 32)).unwrap();
        // 128-bit (VEX.128) saturation/avg/mulhrsw/pmadd — reg + mem.
        a.vpaddsb(xmm2, xmm0, xmm1).unwrap();
        a.vpaddusw(xmm3, xmm0, xmmword_ptr(scratch + 32)).unwrap();
        a.vpsubsw(xmm4, xmm0, xmm1).unwrap();
        a.vpsubusb(xmm5, xmm0, xmm1).unwrap();
        a.vpavgb(xmm6, xmm0, xmm1).unwrap();
        a.vpavgw(xmm7, xmm0, xmmword_ptr(scratch + 32)).unwrap();
        a.vpmulhrsw(xmm8, xmm0, xmm1).unwrap();
        a.vpmaddubsw(xmm9, xmm0, xmm1).unwrap();
        a.vpmaddwd(xmm10, xmm0, xmmword_ptr(scratch + 32)).unwrap();
        // 256-bit (VEX.256) — reg + mem.
        a.vpaddsb(ymm11, ymm0, ymm1).unwrap();
        a.vpmulhrsw(ymm12, ymm0, ymmword_ptr(scratch + 32)).unwrap();
        a.vpmaddubsw(ymm13, ymm0, ymm1).unwrap();
        a.vpmaddwd(ymm14, ymm0, ymm1).unwrap();
        a.hlt().unwrap();
        let bytes = a.assemble(code).unwrap();

        // Scratch: [0..32] = operand A (edges), [32..64] = operand B (edges).
        let a_bytes: [u128; 2] = [
            0x8000_7FFF_0001_FFFF_8080_7F7F_0101_FEFEu128,
            0x7FFF_8000_FFFF_0001_00FF_FF00_8001_017Fu128,
        ];
        let b_bytes: [u128; 2] = [
            0x7FFF_8000_FFFF_0002_017F_8001_00FF_FF01u128,
            0x8000_7FFF_0002_FFFE_8080_7F7F_FEFE_0202u128,
        ];
        let mut scratch_page = vec![0u8; 0x1000];
        scratch_page[0..16].copy_from_slice(&a_bytes[0].to_le_bytes());
        scratch_page[16..32].copy_from_slice(&a_bytes[1].to_le_bytes());
        scratch_page[32..48].copy_from_slice(&b_bytes[0].to_le_bytes());
        scratch_page[48..64].copy_from_slice(&b_bytes[1].to_le_bytes());
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
        let native = run_native(&input).expect("AVX2 host runs the packed-int sweep");
        let interp =
            crate::oracle::run_with_backend(&input, Box::new(x86jit_core::InterpreterBackend));
        assert!(
            crate::compare::compare(&native, &interp, &[]).is_none(),
            "interpreter diverges from the real CPU on the packed-int sweep:\n{:#?}",
            crate::compare::compare(&native, &interp, &[])
        );
    }
}
