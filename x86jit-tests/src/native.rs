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
//! 2. Child arms a `SIGSEGV`/`SIGILL`/… handler on a `sigaltstack` (the guest runs
//!    with its own `RSP`, so the signal frame can't live on the guest stack), then
//!    jumps into the stub, which loads the guest GPRs/flags/XMM and jumps to `entry`.
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
//! Increment 1 captures GPRs, RIP, RFLAGS and the 16 XMM registers — enough to oracle
//! all scalar ALU, shifts, BMI1/2 (VEX-GPR) and SSE2 packed-integer ops. YMM/ZMM
//! upper halves (needed once the fuzzer emits AVX) are a follow-up (read from the
//! signal frame's XSAVE area).

use std::sync::Once;

use iced_x86::code_asm::*;

use crate::oracle::{RunOutcome, VectorInput};
use crate::vector::{CpuSnapshot, ExitKind, MemChunk, SnapFlags};

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
}

const CAP_NONE: u64 = 0;
const CAP_HLT: u64 = 1;
const CAP_FAULT: u64 = 2;

/// A dedicated stack for the fault handler: the guest runs with its own `RSP` (0 in
/// the fuzzer), so the kernel can't push the signal frame onto the guest stack.
const ALTSTACK_LEN: usize = 64 * 1024;
static mut ALTSTACK: [u8; ALTSTACK_LEN] = [0u8; ALTSTACK_LEN];

static INSTALL: Once = Once::new();

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
/// touches raw memory and calls `_exit`. On the guest's terminating `hlt` (a `#GP` →
/// `SIGSEGV` whose faulting byte is `0xf4`) it snapshots the register file; any other
/// signal marks the run unusable so the parent skips it.
extern "C" fn handler(sig: libc::c_int, _info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: `ctx` is a `*mut ucontext_t` (SA_SIGINFO), SHARE is a mapped RW page,
    // and the code page at RIP is mapped, so reading the faulting byte is safe.
    unsafe {
        let uc = &*(ctx as *const libc::ucontext_t);
        let gregs = &uc.uc_mcontext.gregs;
        let rip = gregs[libc::REG_RIP as usize] as u64;
        let cap = &mut *(SHARE as *mut Capture);

        let is_hlt = sig == libc::SIGSEGV && *(rip as *const u8) == 0xf4;
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
        }
        cap.status = CAP_HLT;
        libc::_exit(0);
    }
}

/// Install the fault handler once per process (inherited across `fork`).
fn install_handler() {
    INSTALL.call_once(|| unsafe {
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
    });
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

/// Page-aligned `[start, end)` byte span covering a chunk.
fn chunk_span(c: &MemChunk) -> (u64, usize) {
    let start = c.addr & !(PAGE - 1);
    let end = (c.addr + c.bytes.len() as u64 + PAGE - 1) & !(PAGE - 1);
    (start, (end - start) as usize)
}

/// Assemble the register-load stub: load XMM, flags, and all GPRs from the input
/// block, then `jmp` (indirect, through the input block) to the guest entry. Loading
/// flags via `push`/`popfq` happens *before* `RSP` is overwritten; the final GPR is
/// RAX and the jump reads its target from memory, so no register is clobbered late.
fn assemble_stub() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
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

/// Pack a `SnapFlags` into an `RFLAGS` value (reserved bit 1 set), mirroring the
/// Unicorn oracle's `pack_flags`.
fn pack_flags(f: &SnapFlags) -> u64 {
    let mut r = 0x2u64;
    r |= f.cf as u64;
    r |= (f.pf as u64) << 2;
    r |= (f.af as u64) << 4;
    r |= (f.zf as u64) << 6;
    r |= (f.sf as u64) << 7;
    r |= (f.df as u64) << 10;
    r |= (f.of as u64) << 11;
    r
}

fn unpack_flags(r: u64) -> SnapFlags {
    SnapFlags {
        cf: r & (1 << 0) != 0,
        pf: r & (1 << 2) != 0,
        af: r & (1 << 4) != 0,
        zf: r & (1 << 6) != 0,
        sf: r & (1 << 7) != 0,
        df: r & (1 << 10) != 0,
        of: r & (1 << 11) != 0,
    }
}

/// Run `input` on the real host CPU. Returns `None` when the snippet can't be run
/// natively — a guest VA below `mmap_min_addr`, a control-page collision, an
/// unsupported instruction (`SIGILL`), a non-`hlt` fault, or a timeout — so the
/// caller skips it (the interpreter/Unicorn still cover those).
///
/// Serialized process-wide: the fixed control/guest VAs can host only one run at a
/// time. The whole body holds a mutex.
pub fn run_native(input: &VectorInput) -> Option<RunOutcome> {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    install_handler();

    // Guest VAs must clear mmap_min_addr and not collide with the control window.
    for c in &input.mem_init {
        let (start, len) = chunk_span(c);
        if start < 0x1_0000 || overlaps(start, len, CTRL, (STUB + PAGE - CTRL) as usize) {
            return None;
        }
    }

    // Map control window + guest memory. Guest pages are SHARED so the child's memory
    // writes are visible for read-back; the input/stub pages are private (read-only
    // to the child); the capture page is shared (the handler writes it).
    let mut mapped: Vec<(u64, usize)> = Vec::new();
    let cleanup = |mapped: &[(u64, usize)]| {
        for &(a, l) in mapped {
            unmap(a, l);
        }
    };
    let rw = libc::PROT_READ | libc::PROT_WRITE;
    let rwx = rw | libc::PROT_EXEC;

    if !map_fixed(CTRL, PAGE as usize, rw, false) {
        return None;
    }
    mapped.push((CTRL, PAGE as usize));
    if !map_fixed(SHARE, PAGE as usize, rw, true) {
        cleanup(&mapped);
        return None;
    }
    mapped.push((SHARE, PAGE as usize));
    if !map_fixed(STUB, PAGE as usize, rwx, false) {
        cleanup(&mapped);
        return None;
    }
    mapped.push((STUB, PAGE as usize));

    // Guest chunks (dedup pages: chunks may share one, and double-mapping fails).
    for c in &input.mem_init {
        let (start, len) = chunk_span(c);
        if mapped.iter().any(|&(a, l)| a == start && l == len) {
            continue;
        }
        if !map_fixed(start, len, rwx, true) {
            cleanup(&mapped);
            return None;
        }
        mapped.push((start, len));
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
        // FS/GS base can't be set from user mode without arch_prctl; the fuzzer uses
        // neither, so leave them at the inherited value (0 in the fresh child).
        ((CTRL + IN_RFLAGS) as *mut u64).write(pack_flags(&init.flags));
        ((CTRL + IN_ENTRY) as *mut u64).write(input.entry);
        let xmm = (CTRL + IN_XMM) as *mut u128;
        for (i, &v) in init.xmm.iter().enumerate() {
            xmm.add(i).write(v);
        }
        let stub = assemble_stub();
        std::ptr::copy_nonoverlapping(stub.as_ptr(), STUB as *mut u8, stub.len());

        (*(SHARE as *mut Capture)).status = CAP_NONE;
    }

    // Fork and run the guest in the child.
    // SAFETY: fork; the child path is async-signal-safe until it enters the guest.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        cleanup(&mapped);
        return None;
    }
    if pid == 0 {
        // Child: arm the altstack, cap runtime, jump into the stub. Never returns.
        unsafe {
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
    let cap = unsafe { *(SHARE as *const Capture) };

    let outcome = if cap.status == CAP_HLT {
        Some(RunOutcome {
            cpu: CpuSnapshot {
                gpr: cap.gpr,
                rip: cap.rip,
                flags: unpack_flags(cap.rflags),
                fs_base: input.cpu_init.fs_base,
                gs_base: input.cpu_init.gs_base,
                xmm: cap.xmm,
                // Increment 1 doesn't capture YMM upper halves (see module docs).
                ymm_hi: [0; 16],
            },
            mem: read_back(&input.mem_init),
            exit: ExitKind::Hlt,
        })
    } else {
        None
    };

    cleanup(&mapped);
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
}
