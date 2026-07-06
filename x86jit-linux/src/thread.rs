//! Threaded process driver (go-caddy Phase 2) — the KVM-style split on the embedder
//! side: N guest threads run on N host threads over one shared `Arc<Vm>` and one
//! shared `Arc<Mutex<LinuxShim>>`, servicing syscalls under the shim lock while guest
//! compute runs lock-free. It promotes the proven `x86jit-tests/tests/mt.rs` recipe
//! into the production shim.
//!
//! This is the sibling of [`crate::proc`] (the deferred-fork, single-vcpu driver): a
//! process runs single-threaded/deferred until its first `clone(CLONE_VM)`, at which
//! point it *escalates* — the deferred scheduler hands its owned `(Vm, Vcpu,
//! LinuxShim)` triple to [`run_threaded`], which returns the same
//! [`ProcOutcome`]/[`ProcError`] vocabulary. Escalation is one-directional: a
//! threaded process cannot `fork`/`execve` (P2.8).
//!
//! **Lock order (load-bearing).** shim → futex acquisition is allowed; futex → shim
//! is forbidden; nobody blocks on `ThreadShared::futex_cv` while holding the shim
//! guard. Blocking syscall arms extract what they need, drop the shim guard, block on
//! `ThreadShared`, then re-lock the shim to write the result.
//!
//! Current status: **P2.3 — real futex**. The driver runs the main thread through
//! `shim.handle_mt()`, which surfaces `FUTEX_WAIT`/`FUTEX_WAKE` by value so they block
//! on `ThreadShared` after the shim guard drops. Thread spawning (`clone(CLONE_VM)`)
//! lands in P2.4; until then there is one thread, so a `FUTEX_WAIT` that would block
//! is either a lost race (-EAGAIN) or bounded by its own timeout.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use x86jit_core::{CpuState, Exit, Reg, Vcpu, Vm};

use crate::proc::{ProcError, ProcOutcome};
use crate::shim::{MtClock, SyscallOutcome, ThreadCtx};
use crate::LinuxShim;

/// A guest thread returns to the driver periodically (this many blocks) even when it
/// isn't issuing syscalls, so it notices a sibling's `exit_group`.
const BUDGET: u64 = 50_000;

/// Backstop poll interval for a parked `FUTEX_WAIT`er: even with no wake or timeout,
/// it re-checks `exited` this often. Process exit also `notify_all`s, so this only
/// bounds the worst case, it isn't the primary wake path.
const FUTEX_POLL: Duration = Duration::from_millis(50);

/// errno values a `futex` returns to the guest.
const EAGAIN: u64 = (-11i64) as u64;
const ETIMEDOUT: u64 = (-110i64) as u64;

/// Process-wide thread state, held **outside** the shim mutex so a blocked thread
/// (futex wait, later epoll) never holds the shim lock. Self-synchronizing — every
/// field is atomic or its own lock.
pub struct ThreadShared {
    /// Per-address futex wake generation: a `FUTEX_WAIT`er sleeps until its address's
    /// generation advances (a `FUTEX_WAKE`). Wired in P2.3.
    pub futex: Mutex<HashMap<u64, u64>>,
    pub futex_cv: Condvar,
    /// Set when any thread runs `exit_group` (or the last thread `exit`s): every vcpu
    /// loop stops at its next budget.
    pub exited: AtomicBool,
    /// The process exit code, published by whichever thread ends the process.
    pub exit_code: AtomicU64,
    /// Count of live guest threads. Starts at 1 (main), `+1` per spawned worker, `-1`
    /// as each thread ends. When an `exit(2)` brings it to 0 with no prior
    /// `exit_group`, that thread's code becomes the process status (Linux: the process
    /// lives until its last thread).
    pub alive: AtomicU64,
    /// Spawned worker join handles, drained on process exit. Populated by `clone` (P2.4).
    pub threads: Mutex<Vec<JoinHandle<()>>>,
    /// The process's shared virtual monotonic clock (VCLK, decision-6), cloned from the
    /// shim at `run_threaded`. The driver credits it on expired waits (VCLK-2); inert on
    /// this rung.
    pub clock: Arc<MtClock>,
}

impl ThreadShared {
    fn new(clock: Arc<MtClock>) -> Self {
        ThreadShared {
            futex: Mutex::new(HashMap::new()),
            futex_cv: Condvar::new(),
            exited: AtomicBool::new(false),
            exit_code: AtomicU64::new(0),
            alive: AtomicU64::new(1), // the main thread
            threads: Mutex::new(Vec::new()),
            clock,
        }
    }

    /// `FUTEX_WAIT`: block until this address's wake generation advances (a
    /// `FUTEX_WAKE`), the guest word no longer equals `val`, the process exits, or the
    /// (relative) timeout elapses. Returns the guest `Rax`: `0` woken, `-EAGAIN` on a
    /// value mismatch, `-ETIMEDOUT` on deadline.
    ///
    /// The value re-check happens **under the futex mutex** — that's the linearization
    /// point against `futex_wake`: a waker must take the same lock and bump the
    /// generation, so a wake that races an about-to-sleep waiter is never lost.
    fn futex_wait(&self, vm: &Vm, uaddr: u64, val: u32, timeout: Option<Duration>) -> u64 {
        let mut g = self.futex.lock().unwrap();
        // Already changed → a wake we'd otherwise wait for has effectively happened.
        if read_u32(vm, uaddr) != val {
            return EAGAIN;
        }
        let gen = *g.entry(uaddr).or_insert(0);
        // A garbage-large timespec must not panic `Instant::add`; a deadline that
        // would overflow degrades to an indefinite (poll-backstopped) wait.
        let deadline = timeout.and_then(|d| Instant::now().checked_add(d));
        loop {
            if self.exited.load(Ordering::Relaxed) {
                return 0;
            }
            let wait = match deadline {
                Some(dl) => match dl.checked_duration_since(Instant::now()) {
                    Some(rem) => rem.min(FUTEX_POLL),
                    None => return ETIMEDOUT,
                },
                None => FUTEX_POLL,
            };
            let (ng, _to) = self.futex_cv.wait_timeout(g, wait).unwrap();
            g = ng;
            if *g.get(&uaddr).unwrap_or(&0) != gen {
                return 0; // woken by FUTEX_WAKE on this address
            }
        }
    }

    /// `FUTEX_WAKE`: advance the address's wake generation and release every parked
    /// waiter to re-check its own address. Returns `count` (best-effort, like the
    /// kernel's "woke at most N").
    fn futex_wake(&self, uaddr: u64, count: u64) -> u64 {
        let mut g = self.futex.lock().unwrap();
        *g.entry(uaddr).or_insert(0) += 1;
        self.futex_cv.notify_all();
        count
    }
}

/// A thread faulted (an ISA gap, MMIO, unmapped memory): mark the process exited and
/// wake every parked sibling so their `futex_wait`/`Sleep`/`EpollWait` loops observe
/// `exited` and return, letting the worker joins complete and `run_threaded` surface the
/// error. Without this, a fault on one thread while a sibling is parked hangs the join
/// forever — a real trap masquerading as a futex deadlock (task-132).
fn fault_teardown(shared: &Arc<ThreadShared>) {
    shared.exited.store(true, Ordering::Relaxed);
    shared.futex_cv.notify_all();
}

/// Read a little-endian `u32` from guest memory (0 if unmapped) — the futex word.
fn read_u32(vm: &Vm, addr: u64) -> u32 {
    let mut b = [0u8; 4];
    if vm.read_bytes(addr, &mut b).is_ok() {
        u32::from_le_bytes(b)
    } else {
        0
    }
}

/// Drive an already-loaded process as a threaded process to completion. The caller
/// (the OCI runner, or the deferred scheduler on escalation) has built `vm`, loaded
/// the program, set RIP/RSP on `cpu`, and configured `shim` — this only adds the
/// threaded execution model on top.
///
/// P2.4: `clone(CLONE_VM)` spawns real sibling host threads over the shared Arcs; the
/// main thread runs here. Returns when the process exits and every worker has joined.
pub fn run_threaded(vm: Vm, cpu: Vcpu, shim: LinuxShim) -> Result<ProcOutcome, ProcError> {
    let root_tid = shim.pid;
    // Clone the shared virtual clock out before the shim is Arc-wrapped (VCLK,
    // decision-6): the driver credits it on expired waits, the shim ticks it on reads.
    let clock = shim.mt_clock();
    let vm = Arc::new(vm);
    let shim = Arc::new(Mutex::new(shim));
    let shared = Arc::new(ThreadShared::new(clock));

    // The main thread's identity: its tid is the process pid; its clear_tid is set later
    // if the guest calls `set_tid_address` (musl does at startup).
    let main_ctx = ThreadCtx {
        tid: root_tid,
        clear_tid: 0,
        altstack: Default::default(),
        sigmask: 0,
    };
    let outcome = run_vcpu(&vm, cpu, &shim, &shared, main_ctx);

    // Join every worker the process spawned before reading the outcome, so all stdout
    // and the final exit code are settled. A worker fault is surfaced over a clean main
    // return, but not over a main-thread fault (which is reported first).
    let handles: Vec<_> = std::mem::take(&mut *shared.threads.lock().unwrap());
    for h in handles {
        let _ = h.join();
    }
    outcome?;

    let guard = shim.lock().unwrap();
    Ok(ProcOutcome {
        stdout: guard.stdout.clone(),
        // exit_group publishes through the shim's `exit_code`; the last-thread-`exit`
        // path publishes through `shared.exit_code` (this fallback).
        exit_code: guard
            .exit_code
            .unwrap_or_else(|| shared.exit_code.load(Ordering::Relaxed) as i32),
    })
}

/// How a guest thread's loop ended, deciding the shared epilogue (below).
enum ThreadEnd {
    /// `exit(2)` — only this thread; its code becomes the process status iff it was last.
    Thread(i32),
    /// `exit_group(code)` — the whole process ends now.
    Process(i32),
    /// A sibling already ended the process; this thread observed `exited` and stopped.
    Sibling,
}

/// One guest thread's execution loop: run the vcpu, service each syscall under the shim
/// lock, spawn siblings on `clone`, and stop when the thread or process exits. A budget
/// makes a compute-bound thread return here periodically to observe `exited`.
fn run_vcpu(
    vm: &Arc<Vm>,
    mut cpu: Vcpu,
    shim: &Arc<Mutex<LinuxShim>>,
    shared: &Arc<ThreadShared>,
    mut ctx: ThreadCtx,
) -> Result<(), ProcError> {
    let end = loop {
        if shared.exited.load(Ordering::Relaxed) {
            break ThreadEnd::Sibling;
        }
        match cpu.run(vm, Some(BUDGET)) {
            Exit::BudgetExhausted => continue,
            Exit::Syscall => {
                // Lock the shim only across the syscall decode itself; guest compute
                // above is lock-free, and the blocking ops are serviced *after* the
                // guard drops (lock order: shim → futex).
                let outcome = {
                    let mut s = shim.lock().unwrap();
                    s.handle_mt(&mut cpu, vm, &mut ctx)
                };
                match outcome {
                    SyscallOutcome::Continue => {}
                    SyscallOutcome::FutexWait {
                        uaddr,
                        val,
                        timeout,
                    } => {
                        // Shim guard already dropped; block on `ThreadShared` only. `Rax`
                        // is vcpu-local state, so we set it directly — no shim lock needed.
                        let ret = shared.futex_wait(vm, uaddr, val, timeout);
                        cpu.set_reg(Reg::Rax, ret);
                    }
                    SyscallOutcome::FutexWake { uaddr, count } => {
                        let ret = shared.futex_wake(uaddr, count);
                        cpu.set_reg(Reg::Rax, ret);
                    }
                    SyscallOutcome::Spawn {
                        child_cpu,
                        child_tid,
                        clear_tid,
                    } => {
                        spawn_thread(vm, shim, shared, child_cpu, child_tid, clear_tid);
                    }
                    SyscallOutcome::Sleep(dur) => {
                        // Real, interruptible sleep outside the shim lock: chunk it so a
                        // sibling's process exit ends the sleep promptly. `Rax` was set
                        // to 0 by the shim.
                        let mut remaining = dur;
                        while remaining > std::time::Duration::ZERO
                            && !shared.exited.load(Ordering::Relaxed)
                        {
                            let chunk = remaining.min(FUTEX_POLL);
                            std::thread::sleep(chunk);
                            remaining = remaining.saturating_sub(chunk);
                        }
                    }
                    SyscallOutcome::Yield => std::thread::yield_now(),
                    SyscallOutcome::EpollWait {
                        epfd,
                        events_ptr,
                        maxevents,
                        timeout,
                    } => {
                        // Real host epoll_wait outside the shim lock, chunked so a
                        // sibling's process exit ends the wait promptly (the netpollBreak
                        // eventfd handles guest-initiated wakes instantly, being in the
                        // set). Rax is vcpu-local; set it directly.
                        let raw = epfd.as_raw_fd();
                        let deadline = timeout.and_then(|d| Instant::now().checked_add(d));
                        loop {
                            if shared.exited.load(Ordering::Relaxed) {
                                cpu.set_reg(Reg::Rax, 0);
                                break;
                            }
                            let chunk_ms = match deadline {
                                Some(dl) => match dl.checked_duration_since(Instant::now()) {
                                    Some(rem) => rem.min(FUTEX_POLL).as_millis() as i32,
                                    None => {
                                        cpu.set_reg(Reg::Rax, 0); // timed out
                                        break;
                                    }
                                },
                                None => FUTEX_POLL.as_millis() as i32,
                            };
                            let ret = crate::shim::do_epoll_wait(
                                raw, vm, events_ptr, maxevents, chunk_ms,
                            );
                            // 0 = nothing this chunk → loop; nonzero (ready or -errno) → done.
                            if ret != 0 {
                                cpu.set_reg(Reg::Rax, ret);
                                break;
                            }
                        }
                    }
                    SyscallOutcome::ThreadExit(code) => break ThreadEnd::Thread(code),
                    SyscallOutcome::ProcessExit(code) => break ThreadEnd::Process(code),
                    SyscallOutcome::Unsupported { what } => {
                        // execve/wait/blocking-pipe: no honest errno for a threaded
                        // process — an error, never a host panic (P2.8).
                        fault_teardown(shared);
                        return Err(ProcError::Trapped(format!(
                            "threaded process used {what} — unsupported (P2.8)"
                        )));
                    }
                }
            }
            other => {
                // A fault on any thread (an ISA gap, MMIO, unmapped memory) kills the
                // process, like a fatal signal. Log it loudly at the source (an unknown
                // instruction must scream, not hide), then tear the siblings down so
                // their parked `futex_wait`/`Sleep`/`EpollWait` arms drain and
                // `run_threaded` surfaces this error instead of hanging on the join
                // (task-132).
                crate::report_gap(&other);
                fault_teardown(shared);
                return Err(ProcError::Trapped(format!(
                    "threaded process: {other:?} at rip={:#x}",
                    cpu.reg(Reg::Rip)
                )));
            }
        }
    };

    // Thread-exit epilogue, run outside every lock. First the pthread_join handshake:
    // write 0 to this thread's clear_tid and wake a joiner parked on it.
    if ctx.clear_tid != 0 {
        let _ = vm.write_bytes(ctx.clear_tid, &0u32.to_le_bytes());
        shared.futex_wake(ctx.clear_tid, 1);
    }
    // Then account for this thread leaving and, where it ends the process, publish.
    let last = shared.alive.fetch_sub(1, Ordering::Relaxed) == 1;
    match end {
        ThreadEnd::Process(code) => {
            // exit_group: the shim already set its `exit_code`; mirror it and release
            // every parked waiter so siblings observe `exited`.
            shared.exit_code.store(code as u64, Ordering::Relaxed);
            shared.exited.store(true, Ordering::Relaxed);
            shared.futex_cv.notify_all();
        }
        ThreadEnd::Thread(code) => {
            // Linux: the process lives until its last thread; that thread's status is
            // the process status (unless an exit_group already fixed it).
            if last && !shared.exited.load(Ordering::Relaxed) {
                shared.exit_code.store(code as u64, Ordering::Relaxed);
                shared.exited.store(true, Ordering::Relaxed);
                shared.futex_cv.notify_all();
            }
        }
        ThreadEnd::Sibling => {}
    }
    Ok(())
}

/// Spawn a `clone(CLONE_VM)` child on its own host thread over the shared Arcs. The
/// shim built `child_cpu` (RAX=0, RSP, TLS) already; here we only wrap it in a fresh
/// vcpu (sharing the `Arc<Vm>` code cache) and register its join handle.
fn spawn_thread(
    vm: &Arc<Vm>,
    shim: &Arc<Mutex<LinuxShim>>,
    shared: &Arc<ThreadShared>,
    child_cpu: Box<CpuState>,
    child_tid: u64,
    clear_tid: u64,
) {
    shared.alive.fetch_add(1, Ordering::Relaxed);
    let mut child = vm.new_vcpu();
    child.cpu = *child_cpu;
    let child_ctx = ThreadCtx {
        tid: child_tid,
        clear_tid,
        altstack: Default::default(),
        sigmask: 0,
    };
    let (vm_c, shim_c, shared_c) = (Arc::clone(vm), Arc::clone(shim), Arc::clone(shared));
    let handle = std::thread::spawn(move || {
        // A worker fault has nowhere to propagate; the process still tears down cleanly
        // via `exited`, and the main thread reports its own outcome.
        let _ = run_vcpu(&vm_c, child, &shim_c, &shared_c, child_ctx);
    });
    shared.threads.lock().unwrap().push(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86jit_core::{
        InterpreterBackend, MemConsistency, MemoryModel, Prot, RegionKind, VmConfig,
    };

    const WORD: u64 = 0x1000;

    /// A 4 KiB RW page at [`WORD`] holding a single futex word, initialized to `v`.
    fn tiny_vm(v: u32) -> Vm {
        let mut vm = Vm::with_backend(
            VmConfig {
                memory_model: MemoryModel::Flat { size: 0x2000 },
                consistency: MemConsistency::Fast,
            },
            Box::new(InterpreterBackend),
        );
        vm.map(WORD, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        vm.write_bytes(WORD, &v.to_le_bytes()).unwrap();
        vm
    }

    /// The word already differs from the expected value: a wake we'd wait for has
    /// effectively already happened → -EAGAIN, no block.
    #[test]
    fn wait_value_mismatch_is_eagain() {
        let vm = tiny_vm(7);
        let sh = ThreadShared::new(Arc::new(MtClock::default()));
        assert_eq!(sh.futex_wait(&vm, WORD, 42, None), EAGAIN);
    }

    /// Nobody wakes the waiter and the relative timeout elapses → -ETIMEDOUT.
    #[test]
    fn wait_times_out() {
        let vm = tiny_vm(0);
        let sh = ThreadShared::new(Arc::new(MtClock::default()));
        let start = Instant::now();
        let ret = sh.futex_wait(&vm, WORD, 0, Some(Duration::from_millis(30)));
        assert_eq!(ret, ETIMEDOUT);
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    /// A `FUTEX_WAKE` from a sibling releases the parked waiter → 0.
    #[test]
    fn wake_releases_waiter() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None));
        // Let the waiter park (backstop poll is 50ms; this is well under it), then wake.
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(sh.futex_wake(WORD, 1), 1);
        assert_eq!(waiter.join().unwrap(), 0);
    }

    /// Process exit releases every parked waiter (the `exit_group` path) → 0.
    #[test]
    fn wait_released_by_process_exit() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None));
        std::thread::sleep(Duration::from_millis(20));
        sh.exited.store(true, Ordering::Relaxed);
        sh.futex_cv.notify_all();
        assert_eq!(waiter.join().unwrap(), 0);
    }

    /// task-132: when a thread faults (an ISA gap, MMIO, unmapped memory), `run_vcpu`
    /// runs `fault_teardown`, which must release every *indefinitely* parked sibling so
    /// the worker joins complete and `run_threaded` surfaces the error instead of hanging
    /// forever on the join. This pins that a faulting thread unparks an infinite waiter.
    #[test]
    fn fault_teardown_releases_indefinite_waiter() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        // A worker parked in an indefinite futex wait — the pre-fix hang shape.
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None));
        std::thread::sleep(Duration::from_millis(20));
        fault_teardown(&sh); // what run_vcpu's Err paths now call
        assert_eq!(
            waiter.join().unwrap(),
            0,
            "faulting thread must unpark siblings"
        );
    }
}
