//! Deferred→threaded escalation (task-126). The deferred [`Scheduler`] runs a process
//! single-threaded until its first `clone(CLONE_VM)` — a shared-address-space *thread*,
//! not a `fork`. At that point it can't proceed (the deferred model has no thread
//! substrate), so it *escalates*: it hands its `(Vm, Vcpu, LinuxShim)` to the threaded
//! driver (`run_threaded_escalated`), which services the one peeked-but-un-serviced
//! clone and drives the process to completion on real host threads.
//!
//! Two hand-assembled guests pin the two branches of the peek:
//!  - a `clone(CLONE_VM)` program **escalates** and completes threaded (its child runs
//!    on a real sibling host thread, writes a sentinel, and the parent observes it via
//!    the CLONE_CHILD_CLEARTID futex handshake — impossible on the deferred path);
//!  - a `fork`/`wait4` program **stays on the deferred scheduler** (fork/wait4 are
//!    deferred-only — the threaded driver rejects them as `Unsupported`), so its clean
//!    completion proves it never escalated.
//!
//! Both run on the interpreter and the JIT — escalation is backend-agnostic.

use iced_x86::code_asm::*;
use x86jit_core::{Backend, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;
use x86jit_linux::Scheduler;
use x86jit_tests::syscall::LinuxShim;

const FLAT_SIZE: u64 = 0x10_0000;
const CODE_BASE: u64 = 0x1000;
const DATA_BASE: u64 = 0x4000;
const CTID: u64 = 0x4000; // CLONE_CHILD_CLEARTID word (child-exit handshake)
const STATUS: u64 = 0x4010; // wait4 exit-status word
const CHILD_MSG: u64 = 0x4100; // the byte the clone child prints
const PARENT_MSG: u64 = 0x4108; // the byte the parent prints after the child
const CHILD_STACK_TOP: u64 = 0x8000; // top of the clone child's stack (grows down)

// clone(2) flags.
const CLONE_VM: u32 = 0x0000_0100;
const CLONE_CHILD_CLEARTID: u32 = 0x0020_0000;

// futex ops.
const FUTEX_WAIT: u32 = 0;

/// Guest program whose first action-of-note is `clone(CLONE_VM)` — the escalation
/// trigger. The child (RAX==0) writes "C" and `exit`s; the parent futex-waits on the
/// CLONE_CHILD_CLEARTID word until the child clears it (the threaded driver's
/// pthread-join handshake), then writes "P" and `exit_group`s. Only the threaded driver
/// can run this: the deferred scheduler has no sibling thread to clear the word.
fn clone_program() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut child = a.create_label();
    let mut wait_loop = a.create_label();
    let mut done_wait = a.create_label();

    // ctid = 1 (a nonzero sentinel; the driver writes 0 on child exit).
    a.mov(dword_ptr(CTID), 1u32).unwrap();

    // clone(CLONE_VM | CLONE_CHILD_CLEARTID, CHILD_STACK_TOP, 0, &ctid, 0)
    a.mov(eax, 56u32).unwrap();
    a.mov(edi, CLONE_VM | CLONE_CHILD_CLEARTID).unwrap();
    a.mov(esi, CHILD_STACK_TOP as u32).unwrap();
    a.xor(edx, edx).unwrap(); // ptid = 0
    a.mov(r10d, CTID as u32).unwrap(); // ctid = &ctid
    a.xor(r8d, r8d).unwrap(); // tls = 0
    a.syscall().unwrap();
    a.test(rax, rax).unwrap();
    a.jz(child).unwrap();

    // ---- parent ----
    // Wait until the child clears ctid to 0 (futex handshake), then print "P".
    a.set_label(&mut wait_loop).unwrap();
    a.cmp(dword_ptr(CTID), 0u32).unwrap();
    a.je(done_wait).unwrap();
    // futex(&ctid, FUTEX_WAIT, 1 /*expected*/, 0 /*no timeout*/)
    a.mov(eax, 202u32).unwrap();
    a.mov(edi, CTID as u32).unwrap();
    a.mov(esi, FUTEX_WAIT).unwrap();
    a.mov(edx, 1u32).unwrap();
    a.xor(r10d, r10d).unwrap();
    a.syscall().unwrap();
    a.jmp(wait_loop).unwrap();
    a.set_label(&mut done_wait).unwrap();
    // write(1, PARENT_MSG, 1)
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, 1u32).unwrap();
    a.mov(esi, PARENT_MSG as u32).unwrap();
    a.mov(edx, 1u32).unwrap();
    a.syscall().unwrap();
    // exit_group(0)
    a.mov(eax, 231u32).unwrap();
    a.xor(edi, edi).unwrap();
    a.syscall().unwrap();

    // ---- child (its own stack) ----
    a.set_label(&mut child).unwrap();
    // write(1, CHILD_MSG, 1)
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, 1u32).unwrap();
    a.mov(esi, CHILD_MSG as u32).unwrap();
    a.mov(edx, 1u32).unwrap();
    a.syscall().unwrap();
    // exit(0) — ends just this thread; the driver clears ctid and wakes the parent.
    a.mov(eax, 60u32).unwrap();
    a.xor(edi, edi).unwrap();
    a.syscall().unwrap();

    a.assemble(CODE_BASE).unwrap()
}

/// A `fork`/`wait4` program — a *process*, not a thread. It must stay on the deferred
/// scheduler (fork/wait4 have no threaded-driver support), so its clean completion is
/// proof the peek did not misfire and escalate an ordinary fork.
fn fork_program() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut child = a.create_label();

    // fork()
    a.mov(eax, 57u32).unwrap();
    a.syscall().unwrap();
    a.test(rax, rax).unwrap();
    a.jz(child).unwrap();

    // ---- parent ----
    // wait4(-1, &status, 0, 0), then exit((status >> 8) & 0xff).
    a.mov(eax, 61u32).unwrap();
    a.mov(rdi, -1i64).unwrap();
    a.mov(esi, STATUS as u32).unwrap();
    a.xor(edx, edx).unwrap();
    a.xor(r10d, r10d).unwrap();
    a.syscall().unwrap();
    // write(1, PARENT_MSG, 1)
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, 1u32).unwrap();
    a.mov(esi, PARENT_MSG as u32).unwrap();
    a.mov(edx, 1u32).unwrap();
    a.syscall().unwrap();
    a.mov(eax, dword_ptr(STATUS)).unwrap();
    a.shr(eax, 8u32).unwrap();
    a.and(eax, 0xffu32).unwrap();
    a.mov(edi, eax).unwrap();
    a.mov(eax, 60u32).unwrap();
    a.syscall().unwrap();

    // ---- child ----
    a.set_label(&mut child).unwrap();
    // write(1, CHILD_MSG, 1); exit(7)
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, 1u32).unwrap();
    a.mov(esi, CHILD_MSG as u32).unwrap();
    a.mov(edx, 1u32).unwrap();
    a.syscall().unwrap();
    a.mov(eax, 60u32).unwrap();
    a.mov(edi, 7u32).unwrap();
    a.syscall().unwrap();

    a.assemble(CODE_BASE).unwrap()
}

/// Drive `code` as the root process under the deferred [`Scheduler`], which escalates to
/// the threaded driver on the first `clone(CLONE_VM)` (task-126).
fn drive(
    code: &[u8],
    backend: Box<dyn Backend>,
    make_backend: impl Fn() -> Box<dyn Backend> + 'static,
) -> (Vec<u8>, i32) {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT_SIZE), backend);
    vm.map(CODE_BASE, 0x1000, Prot::RX, RegionKind::Ram)
        .unwrap();
    // One RW data+stack page covers CTID/STATUS/messages and the child stack (all < 0x9000).
    vm.map(DATA_BASE, 0x5000, Prot::RW, RegionKind::Ram)
        .unwrap();
    vm.write_bytes(CODE_BASE, code).unwrap();
    vm.write_bytes(CHILD_MSG, b"C").unwrap();
    vm.write_bytes(PARENT_MSG, b"P").unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE_BASE);

    let out = Scheduler::new(make_backend)
        .run(vm, cpu, LinuxShim::new())
        .expect("process ran to completion");
    (out.stdout, out.exit_code)
}

fn drive_interp(code: &[u8]) -> (Vec<u8>, i32) {
    drive(code, Box::new(InterpreterBackend), || {
        Box::new(InterpreterBackend)
    })
}

fn drive_jit(code: &[u8]) -> (Vec<u8>, i32) {
    drive(code, Box::new(JitBackend::new()), || {
        Box::new(JitBackend::new())
    })
}

/// The escalation case: a `clone(CLONE_VM)` program runs threaded. The child ("C") runs
/// on a real sibling host thread and the parent ("P") observes its exit via the futex
/// handshake — both sentinels present, exit 0. On the deferred path (pre-fix) the clone
/// returned -ENOSYS and this could not complete.
#[test]
fn clone_vm_escalates_and_completes_interp() {
    let (stdout, code) = drive_interp(&clone_program());
    assert_eq!(code, 0, "escalated process exited cleanly");
    assert!(
        stdout.contains(&b'C') && stdout.contains(&b'P'),
        "both the clone child and the parent ran (threaded), got {stdout:?}"
    );
}

#[test]
fn clone_vm_escalates_and_completes_jit() {
    let (stdout, code) = drive_jit(&clone_program());
    assert_eq!(code, 0, "escalated process exited cleanly");
    assert!(
        stdout.contains(&b'C') && stdout.contains(&b'P'),
        "both the clone child and the parent ran (threaded), got {stdout:?}"
    );
}

/// The non-escalation case: a plain `fork`/`wait4` stays on the deferred scheduler.
/// wait4 is deferred-only (the threaded driver rejects it), so this completing —
/// child "C", parent "P", the child's exit code 7 delivered through wait4 — proves the
/// peek did not escalate an ordinary fork.
#[test]
fn fork_stays_on_deferred_scheduler_interp() {
    let (stdout, code) = drive_interp(&fork_program());
    assert_eq!(
        stdout, b"CP",
        "deferred order: child fully, then parent (#11)"
    );
    assert_eq!(
        code, 7,
        "wait4 delivered the child's exit code — deferred path"
    );
}

#[test]
fn fork_stays_on_deferred_scheduler_jit() {
    let (stdout, code) = drive_jit(&fork_program());
    assert_eq!(stdout, b"CP");
    assert_eq!(code, 7);
}

/// A **real** threaded binary through the escalation path (task-126): `pthreads.elf` is a
/// static-musl C program that spawns four pthreads (`clone(CLONE_VM)` + futex mutex/join),
/// each bumping a shared counter 100 000× — deterministic `400000\n` only if guest threads,
/// cross-thread atomics, and the futex handshake all work. `mt.rs` runs it via `run_threaded`
/// directly; here it enters through the *deferred* [`Scheduler`], which must peek the first
/// `clone(CLONE_VM)` and escalate. Its correct output is proof the same real-binary path a
/// non-Go pthreads program (zstd's I/O pool, a shell that execs a threaded binary) takes now
/// works — that program hit the `clone(CLONE_VM) -> -ENOSYS` gap before this change.
mod real_binary {
    use x86jit_core::{Backend, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
    use x86jit_cranelift::JitBackend;
    use x86jit_elf::{load_static_elf, setup_stack};
    use x86jit_linux::Scheduler;
    use x86jit_tests::syscall::LinuxShim;

    const FLAT: u64 = 0x200_0000; // 32 MiB — matches mt.rs
    const HEAP_BASE: u64 = 0x60_0000;
    const STACK_TOP: u64 = 0xf0_0000;
    const MMAP_BASE: u64 = 0x100_0000; // thread stacks come from the shim's mmap arena

    /// Load `pthreads.elf` into a flat VM and drive it through the deferred scheduler,
    /// which escalates to the threaded driver on its first `clone(CLONE_VM)`.
    fn run_pthreads(
        backend: Box<dyn Backend>,
        make: impl Fn() -> Box<dyn Backend> + 'static,
    ) -> Vec<u8> {
        let image = include_bytes!("../programs/pthreads.elf");
        let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
        let entry = load_static_elf(&mut vm, image).expect("load pthreads");
        vm.map(
            HEAP_BASE,
            (FLAT - HEAP_BASE) as usize,
            Prot::RW,
            RegionKind::Ram,
        )
        .unwrap();
        let rsp = setup_stack(&mut vm, STACK_TOP, &[b"pthreads"], &[]).unwrap();

        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, entry);
        cpu.set_reg(Reg::Rsp, rsp);

        // The CLI runner seeds these from the ELF layout; a hand-built flat VM must set
        // them too so the shim's `brk`/anonymous-`mmap` land inside the mapped heap
        // (thread stacks come from the mmap arena). Without them mmap returns -ENOMEM and
        // the clone'd threads get invalid stacks.
        let mut shim = LinuxShim::new();
        shim.brk = HEAP_BASE;
        shim.brk_limit = MMAP_BASE;
        shim.mmap_base = MMAP_BASE;
        shim.mmap_limit = FLAT - 0x1000;

        Scheduler::new(make)
            .run(vm, cpu, shim)
            .expect("pthreads ran to completion")
            .stdout
    }

    #[test]
    fn pthreads_escalates_via_deferred_scheduler_interp() {
        assert_eq!(
            run_pthreads(Box::new(InterpreterBackend), || Box::new(
                InterpreterBackend
            )),
            b"400000\n",
            "the deferred scheduler escalated the real pthreads binary and it ran threaded"
        );
    }

    #[test]
    fn pthreads_escalates_via_deferred_scheduler_jit() {
        assert_eq!(
            run_pthreads(Box::new(JitBackend::new()), || Box::new(JitBackend::new())),
            b"400000\n",
            "JIT: the deferred scheduler escalated the real pthreads binary and it ran threaded"
        );
    }
}
