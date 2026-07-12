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
use crate::shim::{MtClock, ReadTarget, SyscallOutcome, ThreadCtx};
use crate::LinuxShim;

/// x86-64 `clone` syscall number and the flags that classify a `clone`. This is the ONE
/// place that knows the bit logic (task-227): the deferred scheduler's escalation peek,
/// the threaded driver's clone-routing arm, and `handle`'s gap-log arm all delegate here
/// instead of re-declaring these constants. A real *thread* clone sets both `CLONE_VM`
/// (shared address space) AND `CLONE_THREAD` (same thread group); `vfork`/`posix_spawn`
/// set `CLONE_VM|CLONE_VFORK` but NOT `CLONE_THREAD` — those are short-lived shared-VM
/// processes that immediately `execve`, not threads, so they must NOT be treated as
/// threads (neither escalated by the deferred scheduler nor routed to `clone_thread`).
pub(crate) const SYS_CLONE: u64 = 56;
pub(crate) const CLONE_VM: u64 = 0x100;
const CLONE_THREAD: u64 = 0x0001_0000;

/// Is `clone` with flags `rdi` (given `rax`) a real *thread* clone? True iff `rax` is
/// `SYS_CLONE` and both `CLONE_VM` and `CLONE_THREAD` are set. The canonical raw-register
/// predicate: [`is_clone_vm`] (deferred escalation peek) and the threaded driver's clone
/// arm both delegate here so the thread-vs-fork bit logic lives in exactly one function.
pub(crate) fn is_thread_clone(rax: u64, rdi: u64) -> bool {
    rax == SYS_CLONE && rdi & (CLONE_VM | CLONE_THREAD) == (CLONE_VM | CLONE_THREAD)
}

/// Does `cpu` sit on a real thread `clone` — the deferred→threaded escalation trigger
/// (task-126)? Called by the deferred scheduler on `Exit::Syscall` *before* `handle()`
/// services it: `Rax` still holds the syscall nr and `Rdi` the clone flags (the core
/// already advanced RIP past the `syscall`, exactly as a serviced syscall would leave it).
/// Requires `CLONE_VM|CLONE_THREAD` so a `vfork`/`posix_spawn` (`CLONE_VM|CLONE_VFORK`, no
/// `CLONE_THREAD`) stays on the deferred path — escalating it would run the vfork child's
/// `execve` on the threaded driver, which traps. Two register reads and a branch — the
/// single-threaded fast path pays only that, only on the syscall exit.
pub fn is_clone_vm(cpu: &Vcpu) -> bool {
    is_thread_clone(cpu.reg(Reg::Rax), cpu.reg(Reg::Rdi))
}

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

/// The match-any futex bitmask (task-121): a plain `FUTEX_WAKE`/`FUTEX_WAIT` (and the
/// driver's internal clear_tid wake) uses it, so it matches every queued waiter.
const MATCH_ANY: u32 = 0xffff_ffff;

/// Robust-futex list constants (task-122). The kernel walks at most this many list
/// entries so a corrupt/malicious cycle can't hang the exit path.
const ROBUST_LIST_LIMIT: usize = 2048;
/// `FUTEX_OWNER_DIED`: OR'd into a held mutex's futex word on the owner's death, so a
/// surviving locker sees the owner is gone (glibc turns this into `EOWNERDEAD`).
const FUTEX_OWNER_DIED: u32 = 0x4000_0000;

/// One parked `FUTEX_WAIT`er's queue record: a unique id (so a wake can flag a specific
/// waiter and the waiter can find its own state), its bitmask (task-121), and whether a
/// matching `FUTEX_WAKE` has released it.
struct FutexWaiter {
    id: u64,
    bitmask: u32,
    woken: bool,
}

/// The futex wait queue: per address, the parked waiters. Guarded by
/// [`ThreadShared::futex`]; every method runs under that lock. Replaces the pre-task-121
/// per-address generation counter with an explicit waiter list so a `FUTEX_WAKE_BITSET`
/// can wake *only* the waiters whose bitmask intersects the waker's.
#[derive(Default)]
pub struct FutexQueue {
    /// Parked waiters per address, in arrival order.
    by_addr: HashMap<u64, Vec<FutexWaiter>>,
    /// Monotone waiter-id source, unique across the whole queue.
    next_id: u64,
}

impl FutexQueue {
    /// Register a new waiter on `uaddr` and return its unique id.
    fn register(&mut self, uaddr: u64, bitmask: u32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.by_addr.entry(uaddr).or_default().push(FutexWaiter {
            id,
            bitmask,
            woken: false,
        });
        id
    }

    /// Has the waiter `id` on `uaddr` been flagged by a matching wake?
    fn is_woken(&self, uaddr: u64, id: u64) -> bool {
        self.by_addr
            .get(&uaddr)
            .and_then(|w| w.iter().find(|x| x.id == id))
            .is_some_and(|x| x.woken)
    }

    /// Remove the waiter `id` from `uaddr` (on wake, timeout, or exit). Drops the
    /// address's entry once its last waiter leaves so the map doesn't grow unbounded.
    fn deregister(&mut self, uaddr: u64, id: u64) {
        if let Some(w) = self.by_addr.get_mut(&uaddr) {
            w.retain(|x| x.id != id);
            if w.is_empty() {
                self.by_addr.remove(&uaddr);
            }
        }
    }

    /// Flag up to `count` not-yet-woken waiters on `uaddr` whose bitmask ANDs nonzero
    /// with `bitmask`, in arrival order (FIFO, like the kernel's default). Returns the
    /// number flagged.
    fn wake(&mut self, uaddr: u64, count: u64, bitmask: u32) -> u64 {
        let Some(waiters) = self.by_addr.get_mut(&uaddr) else {
            return 0;
        };
        let mut woke = 0u64;
        for w in waiters.iter_mut() {
            if woke >= count {
                break;
            }
            if !w.woken && (w.bitmask & bitmask) != 0 {
                w.woken = true;
                woke += 1;
            }
        }
        woke
    }
}

/// Process-wide thread state, held **outside** the shim mutex so a blocked thread
/// (futex wait, later epoll) never holds the shim lock. Self-synchronizing — every
/// field is atomic or its own lock.
pub struct ThreadShared {
    /// The futex wait queue: per address, the list of parked waiters, each carrying its
    /// bitmask and a `woken` flag (task-121). A `FUTEX_WAIT`er registers itself here and
    /// sleeps until its flag is set (a matching `FUTEX_WAKE`), the guest word changes, the
    /// process exits, or its timeout elapses. A `FUTEX_WAKE` scans the address's waiters
    /// and flags up to `count` whose bitmask ANDs nonzero with the waker's. Wired in P2.3;
    /// bitmask-selective wake added in task-121.
    pub futex: Mutex<FutexQueue>,
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
    /// shim at `run_threaded`. The shim ticks it per clock read; the driver credits it on
    /// expired waits (idle-only CAS gate) via [`credit_expired_wait`](Self::credit_expired_wait).
    pub clock: Arc<MtClock>,
}

impl ThreadShared {
    fn new(clock: Arc<MtClock>) -> Self {
        ThreadShared {
            futex: Mutex::new(FutexQueue::default()),
            futex_cv: Condvar::new(),
            exited: AtomicBool::new(false),
            exit_code: AtomicU64::new(0),
            alive: AtomicU64::new(1), // the main thread
            threads: Mutex::new(Vec::new()),
            clock,
        }
    }

    /// `FUTEX_WAIT` / `FUTEX_WAIT_BITSET`: register this waiter (with its `bitmask`) on
    /// `uaddr`, then block until a matching `FUTEX_WAKE` flags it, the guest word no
    /// longer equals `val`, the process exits, or the (relative) `timeout` elapses.
    /// Returns the guest `Rax`: `0` woken, `-EAGAIN` on a value mismatch, `-ETIMEDOUT`
    /// on deadline. Plain `FUTEX_WAIT` passes `bitmask == 0xffff_ffff` (match-any), so
    /// its behavior is byte-identical to the pre-task-121 generation queue.
    ///
    /// The value re-check and the waiter registration both happen **under the futex
    /// mutex** — that's the linearization point against `futex_wake`: a waker must take
    /// the same lock to scan the queue, so a wake that races an about-to-sleep waiter is
    /// never lost (the waiter's record is already present, or the waker hasn't run yet
    /// and its later `notify_all` releases the now-parked waiter).
    fn futex_wait(
        &self,
        vm: &Vm,
        uaddr: u64,
        val: u32,
        timeout: Option<Duration>,
        bitmask: u32,
    ) -> u64 {
        let mut g = self.futex.lock().unwrap();
        // Already changed → a wake we'd otherwise wait for has effectively happened.
        if read_u32(vm, uaddr) != val {
            return EAGAIN;
        }
        let id = g.register(uaddr, bitmask);
        // A garbage-large timespec must not panic `Instant::add`; a deadline that
        // would overflow degrades to an indefinite (poll-backstopped) wait.
        let deadline = timeout.and_then(|d| Instant::now().checked_add(d));
        let ret = loop {
            if self.exited.load(Ordering::Relaxed) {
                break 0;
            }
            if g.is_woken(uaddr, id) {
                break 0; // released by a matching FUTEX_WAKE on this address
            }
            let wait = match deadline {
                Some(dl) => match dl.checked_duration_since(Instant::now()) {
                    Some(rem) => rem.min(FUTEX_POLL),
                    None => break ETIMEDOUT,
                },
                None => FUTEX_POLL,
            };
            let (ng, _to) = self.futex_cv.wait_timeout(g, wait).unwrap();
            g = ng;
            if g.is_woken(uaddr, id) {
                break 0; // woken by a matching FUTEX_WAKE on this address
            }
        };
        g.deregister(uaddr, id);
        ret
    }

    /// `FUTEX_WAKE` / `FUTEX_WAKE_BITSET`: flag up to `count` parked waiters on `uaddr`
    /// whose stored bitmask ANDs nonzero with the waker's `bitmask`, then release all
    /// parked waiters to re-check their own flags. Returns the number flagged
    /// (best-effort, like the kernel's "woke at most N"). Plain `FUTEX_WAKE` passes
    /// `bitmask == 0xffff_ffff` (match-any), so every waiter on the address matches —
    /// byte-identical to the pre-task-121 generation queue.
    fn futex_wake(&self, uaddr: u64, count: u64, bitmask: u32) -> u64 {
        let mut g = self.futex.lock().unwrap();
        let woke = g.wake(uaddr, count, bitmask);
        // Wake every parked thread so each re-checks *its own* flag: a `wait_timeout`
        // returns the guard, the waiter tests `is_woken`, and a non-matching one parks
        // again. (Broadcast, not targeted — the waiters filter themselves.)
        self.futex_cv.notify_all();
        woke
    }

    /// Credit the shared virtual clock for a wait that blocked for real and then
    /// expired (VCLK, decision-6), where `entry` was peeked before the block. Uses the
    /// **idle-only** CAS gate: the credit lands only if no other guest thread moved the
    /// clock during the wait (`try_advance_from`). A busy process's concurrent reads
    /// carry virtual time forward on their own, so a free-running periodic timer fires
    /// on read-metered virtual time rather than re-coupling the clock to host wall-rate;
    /// an idle process (the M3 progress case — nothing else advances time) gets the full
    /// credit so its timer still fires after one real wait. Callers gate on genuine
    /// expiry only — a wake, readiness, or process-exit credits nothing.
    fn credit_expired_wait(&self, entry: u64, dur: Duration) {
        let _ = self
            .clock
            .try_advance_from(entry, entry + dur.as_nanos() as u64);
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

/// Read a little-endian `u64` from guest memory, or `None` if unmapped — used to walk
/// the robust-futex list, where an unreadable pointer must *stop* the walk (not read as
/// 0, which would look like a valid null terminator or offset).
fn read_u64_opt(vm: &Vm, addr: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    vm.read_bytes(addr, &mut b)
        .ok()
        .map(|()| u64::from_le_bytes(b))
}

/// Walk an exiting thread's robust-futex list and, for each held mutex, OR
/// `FUTEX_OWNER_DIED` into its futex word and wake one waiter (task-122). This is what
/// lets a surviving locker get `EOWNERDEAD` from `pthread_mutex_lock` instead of
/// deadlocking forever on a dead owner.
///
/// The list format (kernel `struct robust_list_head` at `head`):
/// - `+0`  `robust_list *list.next`  — the first list entry (points back to `head` when
///   empty; each entry's `+0` is the next `robust_list*`).
/// - `+8`  `long futex_offset`       — signed byte offset from a `robust_list` node to
///   its futex word (glibc uses a negative offset: the word sits before the node).
/// - `+16` `robust_list *list_op_pending` — a lock/unlock caught mid-operation; handled
///   the same as a list entry so a mutex being (un)locked at the moment of death still
///   gets `FUTEX_OWNER_DIED`.
///
/// The walk follows `next` from `head.list.next` back to `head`, bounded by
/// [`ROBUST_LIST_LIMIT`] so a corrupt/malicious cycle can't hang the exit path. An
/// unreadable pointer stops the walk. A word already carrying `FUTEX_OWNER_DIED` is left
/// (and not re-woken) so a re-walk is idempotent.
fn walk_robust_list(vm: &Vm, shared: &Arc<ThreadShared>, head: u64) {
    // The futex_offset is a signed byte offset applied to each list node to reach its
    // word; it lives at head+8 and is constant for the whole list.
    let Some(offset_raw) = read_u64_opt(vm, head.wrapping_add(8)) else {
        return;
    };
    let futex_offset = offset_raw as i64;

    // Set OWNER_DIED + wake one waiter for the mutex a `robust_list` node at `entry`
    // guards. Skips a word that already has the flag (idempotent re-walk).
    let handle_entry = |entry: u64| {
        let word_addr = (entry as i64).wrapping_add(futex_offset) as u64;
        let word = read_u32(vm, word_addr);
        if word & FUTEX_OWNER_DIED == 0 {
            let _ = vm.write_bytes(word_addr, &(word | FUTEX_OWNER_DIED).to_le_bytes());
            // Wake exactly one waiter (match-any), like the kernel's robust cleanup.
            shared.futex_wake(word_addr, 1, MATCH_ANY);
        }
    };

    // The `list_op_pending` entry (a lock/unlock caught mid-flight) is processed too.
    if let Some(pending) = read_u64_opt(vm, head.wrapping_add(16)) {
        if pending != 0 && pending != head {
            handle_entry(pending);
        }
    }

    // Follow `next` from head.list.next until we return to `head` (empty-list sentinel)
    // or run out of the bounded budget. Each node's `next` is at its `+0`.
    let Some(mut cur) = read_u64_opt(vm, head) else {
        return;
    };
    let mut steps = 0usize;
    while cur != head && cur != 0 && steps < ROBUST_LIST_LIMIT {
        handle_entry(cur);
        match read_u64_opt(vm, cur) {
            Some(next) => cur = next,
            None => break, // unreadable next pointer → stop
        }
        steps += 1;
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
    run_threaded_inner(vm, cpu, shim, false)
}

/// The deferred→threaded escalation entry (task-126). The deferred scheduler peeked a
/// `clone(CLONE_VM)` (via [`is_clone_vm`]) *before* `handle()` serviced it, so the guest
/// sits with `Rax`/`Rdi` still holding the clone args and RIP already advanced past the
/// `syscall`. This entry services that one pending clone up front — via the same
/// `handle_mt`→`clone_thread`→`Spawn` path an in-loop first clone takes, so the parent's
/// `Rax` becomes the child tid, the child thread spawns over the shared Arcs, and the
/// shim flips to mt mode / seeds the virtual clock — then runs the main thread through
/// `run_vcpu` exactly as [`run_threaded`] would.
pub fn run_threaded_escalated(
    vm: Vm,
    cpu: Vcpu,
    shim: LinuxShim,
) -> Result<ProcOutcome, ProcError> {
    run_threaded_inner(vm, cpu, shim, true)
}

/// Shared driver for [`run_threaded`] and [`run_threaded_escalated`]. When `pending_clone`
/// is set, the main thread's first act is to service the already-peeked `clone(CLONE_VM)`
/// (spawning the sibling) before entering its own vcpu loop.
fn run_threaded_inner(
    vm: Vm,
    mut cpu: Vcpu,
    shim: LinuxShim,
    pending_clone: bool,
) -> Result<ProcOutcome, ProcError> {
    let root_tid = shim.pid;
    // Clone the shared virtual clock out before the shim is Arc-wrapped (VCLK,
    // decision-6): the driver credits it on expired waits, the shim ticks it on reads.
    let clock = shim.mt_clock();
    let vm = Arc::new(vm);
    let shim = Arc::new(Mutex::new(shim));
    let shared = Arc::new(ThreadShared::new(clock));

    // The main thread's identity: its tid is the process pid; its clear_tid is set later
    // if the guest calls `set_tid_address` (musl does at startup).
    let mut main_ctx = ThreadCtx {
        tid: root_tid,
        clear_tid: 0,
        altstack: Default::default(),
        sigmask: 0,
        robust_list_head: 0,
        robust_list_len: 0,
    };
    // Escalation handoff: consume the one clone the deferred scheduler peeked but left
    // un-serviced. `handle_mt` routes it to `clone_thread` (flips to mt mode, seeds the
    // clock, sets the parent's `Rax` to the child tid, returns `Spawn`); we spawn the
    // sibling here, then the main thread falls into `run_vcpu` past the `syscall`.
    if pending_clone {
        let outcome = {
            let mut s = shim.lock().unwrap();
            s.handle_mt(&mut cpu, &vm, &mut main_ctx)
        };
        match outcome {
            SyscallOutcome::Spawn {
                child_cpu,
                child_tid,
                clear_tid,
            } => spawn_thread(&vm, &shim, &shared, child_cpu, child_tid, clear_tid),
            // The peek guarantees a `clone(CLONE_VM)`, so `handle_mt` always returns
            // `Spawn` here. Anything else means the peek and the handler disagree — a
            // logic bug, not a guest-reachable state.
            _ => {
                return Err(ProcError::Trapped(
                    "escalation handoff: expected clone Spawn from handle_mt".into(),
                ));
            }
        }
    }
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
        stderr: guard.stderr.clone(), // task-129: surface fd-2 for diagnostics
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

/// Park the calling thread until `ready()` (a blocking `read`/`accept` target becomes
/// serviceable) or the process exits, polling in `FUTEX_POLL`-sized chunks — the same
/// exit-observing cap the `EpollWait`/`Sleep`/`futex_wait` loops use, so a parked reader
/// never misses a sibling's `exit_group` and never busy-spins (task-125). Returns `true`
/// if `ready()` fired, `false` if the process exited first. There's no host fd to sleep
/// *on* for an in-process pipe, so a bounded `FUTEX_POLL` sleep between probes is the
/// deterministic, non-hot wait (a sibling `write` is observed within one chunk).
fn block_until(shared: &Arc<ThreadShared>, mut ready: impl FnMut() -> bool) -> bool {
    loop {
        if shared.exited.load(Ordering::Relaxed) {
            return false;
        }
        if ready() {
            return true;
        }
        std::thread::sleep(FUTEX_POLL);
    }
}

/// Is a parked [`ReadTarget`] serviceable now? A pipe is ready once it has data *or* its
/// last writer closed (a drained, writer-less pipe is EOF, not a block); a host fd is ready
/// once it `poll`s readable (or hung up). The mirror of the shim's `read_would_block` probe,
/// evaluated from the driver side while parked outside the shim lock (task-125).
fn read_target_ready(target: &ReadTarget) -> bool {
    match target {
        ReadTarget::Pipe(rc) => {
            let b = rc.lock().unwrap();
            !b.data.is_empty() || b.writers == 0
        }
        ReadTarget::Host(rc) => crate::shim::fd_readable(rc.as_raw_fd()),
    }
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
        // `guarded_run` converts a host SIGSEGV from a guard page (an in-span-unmapped
        // access under the JIT — e.g. a Go nil-deref) into a resumable
        // `Exit::UnmappedMemory`, matching the interpreter (doc-30, task-127).
        match crate::sigsegv::guarded_run(&mut cpu, vm, Some(BUDGET)) {
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
                        bitmask,
                    } => {
                        // Shim guard already dropped; block on `ThreadShared` only. `Rax`
                        // is vcpu-local state, so we set it directly — no shim lock needed.
                        let entry = shared.clock.peek();
                        let ret = shared.futex_wait(vm, uaddr, val, timeout, bitmask);
                        // A real timeout expiry credits its full duration to the shared
                        // virtual clock (VCLK, decision-6); a wake, value-mismatch, or
                        // process-exit return advances nothing.
                        if ret == ETIMEDOUT {
                            if let Some(to) = timeout {
                                shared.credit_expired_wait(entry, to);
                            }
                        }
                        cpu.set_reg(Reg::Rax, ret);
                    }
                    SyscallOutcome::FutexWake {
                        uaddr,
                        count,
                        bitmask,
                    } => {
                        let ret = shared.futex_wake(uaddr, count, bitmask);
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
                        let entry = shared.clock.peek();
                        let mut remaining = dur;
                        while remaining > std::time::Duration::ZERO
                            && !shared.exited.load(Ordering::Relaxed)
                        {
                            let chunk = remaining.min(FUTEX_POLL);
                            std::thread::sleep(chunk);
                            remaining = remaining.saturating_sub(chunk);
                        }
                        // Credit virtual time only when the sleep ran to full term (VCLK,
                        // decision-6): a sibling-exit early-out advances nothing. `advance_to`
                        // (fetch_max) lets concurrent sleepers overlap like real time, not sum.
                        if remaining == std::time::Duration::ZERO {
                            shared.credit_expired_wait(entry, dur);
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
                        let entry = shared.clock.peek();
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
                                                                  // Real deadline expiry credits its full duration to
                                                                  // the shared virtual clock (VCLK, decision-6); a
                                                                  // readiness or exit return advances nothing.
                                        if let Some(to) = timeout {
                                            shared.credit_expired_wait(entry, to);
                                        }
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
                    SyscallOutcome::BlockingRead { target, buf, len } => {
                        // Park outside the shim lock until the read target is ready (data,
                        // or EOF), chunked at `FUTEX_POLL` so a sibling's process exit ends
                        // the wait promptly — the same shape as the `EpollWait`/`Sleep`
                        // arms. On readiness, re-take the shim lock and complete the read
                        // (scratch + guest write live in the shim). `Rax` is vcpu-local.
                        //
                        // Re-park loop (task-230): `read_target_ready` is level-triggered, so
                        // one ready event can wake two threads sharing a blocking host fd; the
                        // loser finds the fd drained and `read_ready` returns `None`. Rather
                        // than block a `libc::read` under the shim lock (the deadlock), loop
                        // back to `block_until` for the *next* readiness event. `block_until`
                        // observes `shared.exited`, so a re-parking reader still exits cleanly.
                        loop {
                            if !block_until(shared, || read_target_ready(&target)) {
                                // Process exit ended the wait; a bare 0 (EOF-like) is harmless
                                // — this thread is stopping.
                                cpu.set_reg(Reg::Rax, 0);
                                break;
                            }
                            let done = {
                                let mut s = shim.lock().unwrap();
                                s.read_ready(vm, &target, buf, len)
                            };
                            match done {
                                Some(ret) => {
                                    cpu.set_reg(Reg::Rax, ret);
                                    break;
                                }
                                None => continue, // lost the readiness race → re-park
                            }
                        }
                    }
                    SyscallOutcome::BlockingAccept {
                        listen,
                        addr_ptr,
                        addrlen_ptr,
                        flags,
                    } => {
                        // Park until the listen fd has a pending connection, chunked so a
                        // process exit ends the wait. On readiness, re-take the shim lock and
                        // do the real `accept4` + fd-table install there (fd allocation is
                        // shim state — the mutation must happen under the lock, task-125).
                        //
                        // Re-park loop (task-230): `fd_readable` is level-triggered, so one
                        // pending connection can wake two threads sharing a blocking listen
                        // fd; the loser finds it already accepted and `accept_ready` returns
                        // `None`. Rather than block a `libc::accept4` under the shim lock (the
                        // deadlock), loop back to `block_until` for the *next* connection.
                        // `block_until` observes `shared.exited`, so a re-parking acceptor
                        // still exits cleanly.
                        let raw = listen.as_raw_fd();
                        loop {
                            if !block_until(shared, || crate::shim::fd_readable(raw)) {
                                cpu.set_reg(Reg::Rax, 0);
                                break;
                            }
                            let done = {
                                let mut s = shim.lock().unwrap();
                                s.accept_ready(vm, raw, addr_ptr, addrlen_ptr, flags)
                            };
                            match done {
                                Some(ret) => {
                                    cpu.set_reg(Reg::Rax, ret);
                                    break;
                                }
                                None => continue, // lost the connection race → re-park
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

    // Thread-exit epilogue, run outside every lock. First the robust-futex list walk
    // (task-122): Linux runs this on *every* thread exit before the clear_tid handshake,
    // so a mutex the dying thread still held gets FUTEX_OWNER_DIED set and a waiter woken
    // (a surviving locker then gets EOWNERDEAD instead of deadlocking on a dead owner).
    if ctx.robust_list_head != 0 {
        walk_robust_list(vm, shared, ctx.robust_list_head);
    }
    // Then the pthread_join handshake: write 0 to this thread's clear_tid and wake a
    // joiner parked on it.
    if ctx.clear_tid != 0 {
        let _ = vm.write_bytes(ctx.clear_tid, &0u32.to_le_bytes());
        shared.futex_wake(ctx.clear_tid, 1, MATCH_ANY);
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
        // A clone(CLONE_VM) child starts with an empty robust list; it installs its own
        // via set_robust_list at pthread startup (task-122).
        robust_list_head: 0,
        robust_list_len: 0,
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
    use x86jit_core::{InterpreterBackend, Prot, RegionKind, VmConfig};

    const WORD: u64 = 0x1000;

    /// A 4 KiB RW page at [`WORD`] holding a single futex word, initialized to `v`.
    fn tiny_vm(v: u32) -> Vm {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
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
        assert_eq!(sh.futex_wait(&vm, WORD, 42, None, MATCH_ANY), EAGAIN);
    }

    /// Nobody wakes the waiter and the relative timeout elapses → -ETIMEDOUT.
    #[test]
    fn wait_times_out() {
        let vm = tiny_vm(0);
        let sh = ThreadShared::new(Arc::new(MtClock::default()));
        let start = Instant::now();
        let ret = sh.futex_wait(&vm, WORD, 0, Some(Duration::from_millis(30)), MATCH_ANY);
        assert_eq!(ret, ETIMEDOUT);
        assert!(start.elapsed() >= Duration::from_millis(20));
    }

    /// A `FUTEX_WAKE` from a sibling releases the parked waiter → 0.
    #[test]
    fn wake_releases_waiter() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None, MATCH_ANY));
        // Let the waiter park (backstop poll is 50ms; this is well under it), then wake.
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(sh.futex_wake(WORD, 1, MATCH_ANY), 1);
        assert_eq!(waiter.join().unwrap(), 0);
    }

    /// Process exit releases every parked waiter (the `exit_group` path) → 0.
    #[test]
    fn wait_released_by_process_exit() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None, MATCH_ANY));
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
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None, MATCH_ANY));
        std::thread::sleep(Duration::from_millis(20));
        fault_teardown(&sh); // what run_vcpu's Err paths now call
        assert_eq!(
            waiter.join().unwrap(),
            0,
            "faulting thread must unpark siblings"
        );
    }

    /// VCLK-2: a futex wait that blocks for real and expires credits the shared virtual
    /// clock by at least its timeout (the driver's `ret == ETIMEDOUT` path), so a Go
    /// timer/deadline loop makes virtual progress even while the backend runs slowly.
    #[test]
    fn expired_futex_timeout_credits_clock() {
        let vm = tiny_vm(0);
        let sh = ThreadShared::new(Arc::new(MtClock::default()));
        let entry = sh.clock.peek();
        let to = Duration::from_millis(30);
        let ret = sh.futex_wait(&vm, WORD, 0, Some(to), MATCH_ANY);
        assert_eq!(ret, ETIMEDOUT, "no waker → the deadline expires");
        // The driver credits only on ETIMEDOUT (mirrored here).
        if ret == ETIMEDOUT {
            sh.credit_expired_wait(entry, to);
        }
        assert!(
            sh.clock.peek() - entry >= to.as_nanos() as u64,
            "an expired timeout advances the shared clock at least its duration"
        );
    }

    /// VCLK-2: a futex wait released by a `FUTEX_WAKE` before its timeout returns 0, so
    /// the driver's gate credits nothing — a wake is guest progress the per-read quantum
    /// already accounts for; only real elapsed waits add virtual time.
    #[test]
    fn woken_futex_does_not_credit_clock() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let entry = sh.clock.peek();
        let to = Duration::from_secs(10); // long enough that only the wake can end it
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        let waiter = std::thread::spawn(move || {
            let e = sh2.clock.peek();
            let ret = sh2.futex_wait(&vm2, WORD, 0, Some(to), MATCH_ANY);
            if ret == ETIMEDOUT {
                sh2.credit_expired_wait(e, to);
            }
            ret
        });
        std::thread::sleep(Duration::from_millis(20));
        sh.futex_wake(WORD, 1, MATCH_ANY);
        assert_eq!(
            waiter.join().unwrap(),
            0,
            "the wake, not the timeout, ends it"
        );
        assert_eq!(
            sh.clock.peek(),
            entry,
            "a woken wait credits nothing to the shared clock"
        );
    }

    /// VCLK-2 CAS gate (decision-6 M3): if another guest read moves the shared clock
    /// while a wait is outstanding, the expiry credit is a no-op — a busy process's
    /// periodic timer fires on read-metered virtual time, never re-coupling the clock
    /// to host wall-rate. This is the inverse of `expired_futex_timeout_credits_clock`
    /// (an idle wait, whose credit lands).
    #[test]
    fn busy_process_expiry_does_not_credit() {
        let sh = ThreadShared::new(Arc::new(MtClock::default()));
        let entry = sh.clock.peek();
        // A concurrent worker ticks the clock during the (would-be) wait.
        sh.clock.tick(500);
        let moved = sh.clock.peek();
        // The expiry credit must not land — the clock already advanced past `entry`.
        sh.credit_expired_wait(entry, Duration::from_millis(30));
        assert_eq!(
            sh.clock.peek(),
            moved,
            "a clock moved since entry rejects the expiry credit"
        );
    }

    /// VCLK-2: the shared clock is one atomic, so concurrent readers (each `tick`ing on a
    /// clock read) each observe their own strictly increasing values and the final value
    /// is at least every tick summed — no lost updates, monotone under contention.
    #[test]
    fn concurrent_clock_readers_stay_monotone() {
        const THREADS: u64 = 8;
        const ITERS: u64 = 10_000;
        const Q: u64 = 10_000; // a stand-in quantum (shim's MT_CLOCK_TICK_NS is private)
        let clock = Arc::new(MtClock::default());
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let c = Arc::clone(&clock);
                std::thread::spawn(move || {
                    let mut last = 0;
                    for _ in 0..ITERS {
                        let now = c.tick(Q);
                        assert!(now > last, "own reads strictly increase");
                        last = now;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            clock.peek(),
            THREADS * ITERS * Q,
            "every tick landed — no lost fetch_add"
        );
    }

    /// Which credit rule a periodic timer uses on an expired wait — the axis under test.
    #[derive(Clone, Copy)]
    enum Credit {
        /// Production (decision-6 M3): advance only if the process was time-silent
        /// (`MtClock::try_advance_from`, a `compare_exchange`).
        IdleCas,
        /// The rejected re-coupling rule: unconditional monotone bump
        /// (`MtClock::advance_to`, a `fetch_max`).
        FetchMax,
    }

    /// Replay one long-span busy-process interleaving and return `(final_clock,
    /// total_worker_ticks)`. Each cycle models a free-running periodic timer (Go's
    /// sysmon / a `time.Tick`) whose wait is overlapped by a concurrent worker clock
    /// read: a two-phase barrier orders the worker's `tick` strictly between the
    /// timer's `peek(entry)` and its credit, so the process is *never* time-silent
    /// across a wait — the exact busy case the CAS gate must defeat. Deterministic
    /// (no real sleeps), so it is stable across runs and CPU load.
    fn run_busy_periodic(policy: Credit, cycles: u64, q: u64, credit_ns: u64) -> (u64, u64) {
        let clock = Arc::new(MtClock::default());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let ticks = Arc::new(AtomicU64::new(0));

        let worker = {
            let (clock, barrier, ticks) = (clock.clone(), barrier.clone(), ticks.clone());
            std::thread::spawn(move || {
                for _ in 0..cycles {
                    barrier.wait(); // B1: the timer has peeked `entry`
                    clock.tick(q); // a concurrent guest read carries virtual time forward
                    ticks.fetch_add(1, Ordering::Relaxed);
                    barrier.wait(); // B2: release the timer to apply its credit
                }
            })
        };

        for _ in 0..cycles {
            let entry = clock.peek();
            barrier.wait(); // B1
            barrier.wait(); // B2: the worker has now ticked, so value != entry
            let target = entry + credit_ns;
            match policy {
                Credit::IdleCas => {
                    clock.try_advance_from(entry, target);
                }
                Credit::FetchMax => {
                    clock.advance_to(target);
                }
            }
        }
        worker.join().unwrap();
        (clock.peek(), ticks.load(Ordering::Relaxed))
    }

    /// The honest task-134 acceptance gate (decision-6): the SAME busy interleaving —
    /// a periodic timer whose every wait is overlapped by a worker read, replayed over
    /// a long span — DISCRIMINATES the two credit rules. Under the idle-only CAS gate
    /// (production) no expiry re-couples the clock: virtual time stays exactly
    /// read-metered (the worker ticks), so a wall-coupled injection of zero keeps a
    /// virtual deadline intact. Under a `fetch_max` credit each expiry ratchets the
    /// clock at wall-rate, injecting far more than the deadline — the deadline blows.
    /// This is the multi-cycle sibling of `busy_process_expiry_does_not_credit` (one
    /// cycle) and the inverse of `expired_futex_timeout_credits_clock` (the idle case,
    /// where the CAS credit *does* land). Deterministic → non-flaky, load-invariant.
    #[test]
    fn busy_periodic_timer_discriminates_cas_from_fetch_max() {
        const CYCLES: u64 = 64;
        const Q: u64 = 100; // the mt per-read quantum (shim's MT_CLOCK_TICK_NS)
        const PERIOD_NS: u64 = 10_000_000; // 10 ms — a realistic sysmon/ticker interval
                                           // A guest deadline measured in virtual (read-metered) ns. Read-metered progress
                                           // over the span is CYCLES*Q = 6.4 µs, far under it; a wall-coupled re-coupling
                                           // injects ~CYCLES*PERIOD (~640 ms), far over it.
        const DEADLINE_NS: u64 = CYCLES * PERIOD_NS / 2; // 320 ms

        // Idle-only CAS gate: the worker's read defeats every expiry credit, so the
        // clock advances by exactly the ticks — zero wall-coupled injection.
        let (cas_final, cas_ticks) = run_busy_periodic(Credit::IdleCas, CYCLES, Q, PERIOD_NS);
        assert_eq!(cas_ticks, CYCLES, "the worker ticked once per cycle");
        assert_eq!(
            cas_final,
            CYCLES * Q,
            "CAS gate: virtual time is exactly read-metered (no credit re-coupled)"
        );
        let cas_injected = cas_final - Q * cas_ticks;
        assert_eq!(cas_injected, 0, "CAS gate: zero wall-coupled injection");
        assert!(
            cas_injected < DEADLINE_NS,
            "CAS gate: the virtual deadline holds"
        );

        // fetch_max: every expiry lands and ratchets the clock at wall-rate.
        let (fm_final, fm_ticks) = run_busy_periodic(Credit::FetchMax, CYCLES, Q, PERIOD_NS);
        assert_eq!(
            fm_ticks, CYCLES,
            "same worker ticks — only the credit rule differs"
        );
        let fm_injected = fm_final - Q * fm_ticks;
        assert!(
            fm_injected >= DEADLINE_NS,
            "fetch_max re-couples the clock to wall-rate: injected {fm_injected} ns \
             blows the {DEADLINE_NS} ns deadline the CAS gate held"
        );
    }

    /// AC#3 tripwire (doc-28 I3/I5, the 30 ms micro-repro): a guest
    /// `for time.Since(start) < 30ms { n++ }` spin terminates with `n > 0`, because
    /// every clock read advances virtual time by the per-read quantum — the loop can't
    /// livelock on a clock that only moves when read. Modeled at the `MtClock` level
    /// (backend-agnostic: interp and JIT share this clock, so it holds on both).
    #[test]
    fn read_metered_deadline_spin_terminates() {
        const Q: u64 = 100;
        const DEADLINE_NS: u64 = 30_000_000; // 30 ms
        let clock = MtClock::default();
        let start = clock.peek();
        let mut n = 0u64;
        // Each iteration is one guest clock read (`tick`), which advances time.
        while clock.tick(Q) - start < DEADLINE_NS {
            n += 1;
            assert!(
                n < DEADLINE_NS / Q + 2,
                "the read-metered clock must cross 30 ms"
            );
        }
        assert!(
            n > 0,
            "the spin ran, then read-metered time crossed the deadline"
        );
        // The read that reaches 30 ms exits the loop without counting, so the body ran
        // for every read strictly under the deadline: (30 ms / quantum) − 1.
        assert_eq!(n, DEADLINE_NS / Q - 1, "n = reads strictly under 30 ms");
    }

    /// task-121: a `WAIT_BITSET` waiter is woken **only** by a `WAKE_BITSET` whose
    /// bitmask intersects its own. A non-intersecting wake leaves it parked; a later
    /// intersecting wake releases it. Deterministic (no reliance on timing to prove the
    /// negative — the non-matching wake's return count is 0 and the waiter is asserted
    /// still parked via a bounded, then intersecting, release).
    #[test]
    fn wake_bitset_is_selective() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        // A waiter with bitmask 0b0010 (only a wake touching bit 1 should release it).
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None, 0b0010));
        // Let it park.
        std::thread::sleep(Duration::from_millis(20));
        // A non-intersecting wake (bit 0) must flag nobody — return 0 — and leave the
        // waiter parked. `wake` returns the number flagged, so this is a deterministic
        // assertion of selectivity, not a timing race.
        assert_eq!(
            sh.futex_wake(WORD, 1, 0b0001),
            0,
            "a non-intersecting WAKE_BITSET flags no waiter"
        );
        // The intersecting wake (bit 1) flags exactly the waiter and releases it.
        assert_eq!(
            sh.futex_wake(WORD, 1, 0b0110),
            1,
            "an intersecting WAKE_BITSET flags the waiter"
        );
        assert_eq!(waiter.join().unwrap(), 0, "the matching wake released it");
    }

    /// task-121: a `WAIT_BITSET` with an absolute deadline already in the past times out
    /// immediately. The shim converts the absolute deadline to a relative bound before
    /// this point, so a past deadline arrives as `Duration::ZERO` — the wait must return
    /// `-ETIMEDOUT` without blocking (asserted by an upper time bound).
    #[test]
    fn wait_bitset_past_deadline_times_out_immediately() {
        let vm = tiny_vm(0);
        let sh = ThreadShared::new(Arc::new(MtClock::default()));
        let start = Instant::now();
        // A zero relative timeout is what `abs_deadline_to_rel` yields for a past deadline.
        let ret = sh.futex_wait(&vm, WORD, 0, Some(Duration::ZERO), MATCH_ANY);
        assert_eq!(
            ret, ETIMEDOUT,
            "a past absolute deadline → immediate -ETIMEDOUT"
        );
        assert!(
            start.elapsed() < Duration::from_millis(20),
            "a past deadline must not actually block"
        );
    }

    /// task-121: a plain `FUTEX_WAKE` (match-any, `0xffff_ffff`) still releases a
    /// `WAIT_BITSET` waiter parked with a *narrow* bitmask — the unification means a plain
    /// waker matches every queued waiter, so a mixed WAIT_BITSET/plain-WAKE program (glibc
    /// on some paths) stays correct.
    #[test]
    fn plain_wake_releases_bitset_waiter() {
        let vm = Arc::new(tiny_vm(0));
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        let waiter = std::thread::spawn(move || sh2.futex_wait(&vm2, WORD, 0, None, 0b0001));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            sh.futex_wake(WORD, 1, MATCH_ANY),
            1,
            "a match-any (plain) wake releases a narrow-bitmask waiter"
        );
        assert_eq!(waiter.join().unwrap(), 0);
    }

    /// task-122: `walk_robust_list` on an exiting thread ORs `FUTEX_OWNER_DIED` into the
    /// futex word of a held mutex and wakes one waiter (the `EOWNERDEAD` handoff). This
    /// exercises the walk-on-exit for the common single-entry list, at the ThreadShared
    /// level, deterministically (the waiter is proved released by its join returning).
    #[test]
    fn robust_list_walk_sets_owner_died_and_wakes() {
        // Layout in the 0x1000 page (a realistic glibc-shaped node — the futex word sits
        // at a nonzero offset from the `robust_list` node, so the `next` pointer at the
        // node's +0 and the word don't overlap):
        //   head  @ 0x1000: { next=ENTRY, futex_offset=8, pending=0 }
        //   entry @ 0x1040: { next=head }         — the `next` pointer at +0
        //   word  @ 0x1048: entry + futex_offset  — the mutex's futex word
        // A locker holds the mutex (word = owner tid, say 5) and a sibling parks on it.
        // On the owner's exit the walk must set OWNER_DIED in the word and wake the sibling.
        const HEAD: u64 = 0x1000;
        const ENTRY: u64 = 0x1040;
        const FUTEX_OFFSET: u64 = 8;
        let word_addr = ENTRY + FUTEX_OFFSET;
        let owner_tid: u32 = 5;
        let vm = {
            let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
            vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
            // head.next = ENTRY, head.futex_offset = 8, head.pending = 0
            vm.write_bytes(HEAD, &ENTRY.to_le_bytes()).unwrap();
            vm.write_bytes(HEAD + 8, &FUTEX_OFFSET.to_le_bytes())
                .unwrap();
            vm.write_bytes(HEAD + 16, &0u64.to_le_bytes()).unwrap();
            // entry.next = HEAD (single-entry list, loops back to head)
            vm.write_bytes(ENTRY, &HEAD.to_le_bytes()).unwrap();
            // The futex word holds the owner tid = locked.
            vm.write_bytes(word_addr, &owner_tid.to_le_bytes()).unwrap();
            Arc::new(vm)
        };

        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        // A sibling parks on the mutex's futex word, waiting for a wake.
        let (vm2, sh2) = (Arc::clone(&vm), Arc::clone(&sh));
        let waiter = std::thread::spawn(move || {
            // The word currently == owner_tid; wait on that value.
            sh2.futex_wait(&vm2, word_addr, owner_tid, None, MATCH_ANY)
        });
        std::thread::sleep(Duration::from_millis(20));

        // The owner thread exits → the driver walks its robust list.
        walk_robust_list(&vm, &sh, HEAD);

        // The futex word now carries FUTEX_OWNER_DIED (OR'd onto the tid).
        let word = read_u32(&vm, word_addr);
        assert_eq!(
            word & FUTEX_OWNER_DIED,
            FUTEX_OWNER_DIED,
            "the walk set FUTEX_OWNER_DIED in the held mutex's word"
        );
        assert_eq!(
            word & 0x3fff_ffff,
            owner_tid,
            "the owner tid bits are preserved (only the flag is OR'd in)"
        );
        // The parked sibling was woken (value changed under it → the wait returns 0).
        assert_eq!(
            waiter.join().unwrap(),
            0,
            "the walk woke the sibling (EOWNERDEAD handoff)"
        );
    }

    /// task-122: a corrupt robust list that cycles without ever returning to `head` must
    /// not hang the exit path — the walk is bounded by `ROBUST_LIST_LIMIT`. This builds a
    /// two-node cycle (A→B→A) that never reaches `head` and asserts the walk terminates.
    #[test]
    fn robust_list_walk_is_bounded_against_cycles() {
        // A nonzero futex_offset keeps each node's futex word clear of its `next` pointer
        // (+0), so setting OWNER_DIED doesn't corrupt the chain — the cycle stays intact
        // and the walk must rely on the iteration bound to terminate.
        const HEAD: u64 = 0x1000;
        const A: u64 = 0x1040;
        const B: u64 = 0x1080;
        const FUTEX_OFFSET: u64 = 0x100; // word well clear of the +0 next pointer
        let vm = {
            let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
            vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
            vm.write_bytes(HEAD, &A.to_le_bytes()).unwrap(); // head.next = A
            vm.write_bytes(HEAD + 8, &FUTEX_OFFSET.to_le_bytes())
                .unwrap(); // futex_offset
            vm.write_bytes(HEAD + 16, &0u64.to_le_bytes()).unwrap(); // pending = 0
            vm.write_bytes(A, &B.to_le_bytes()).unwrap(); // A.next = B
            vm.write_bytes(B, &A.to_le_bytes()).unwrap(); // B.next = A (cycle, never head)
            Arc::new(vm)
        };
        let sh = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        // If the walk were unbounded this would spin forever; the bound makes it return.
        walk_robust_list(&vm, &sh, HEAD);
        // Reaching here (the test didn't hang) is the assertion. Sanity-check the flag was
        // set on at least the first node the walk touched (its word at A + futex_offset).
        assert_eq!(
            read_u32(&vm, A + FUTEX_OFFSET) & FUTEX_OWNER_DIED,
            FUTEX_OWNER_DIED,
            "the bounded walk still processed nodes before giving up on the cycle"
        );
    }

    /// task-125 AC: a threaded read that blocks on an empty pipe with a live writer parks
    /// (via `block_until` — the driver's `BlockingRead` wait), and a sibling `write` that
    /// fills the buffer *resumes* it with the data. Proves the yield + resume: the parked
    /// reader is still parked while the pipe is empty, then completes once the sibling
    /// writes — no busy-spin (a bounded `FUTEX_POLL` poll observes the write within a
    /// chunk), no lock held across the block (the buffer is a plain `Arc<Mutex>`).
    #[test]
    fn blocking_pipe_read_yields_then_resumes_with_data() {
        use crate::shim::PipeBuf;
        use std::collections::VecDeque;

        let pipe = Arc::new(Mutex::new(PipeBuf::with(VecDeque::new(), 1, 1)));
        let shared = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let target = ReadTarget::Pipe(Arc::clone(&pipe));

        // A worker parks in the driver's block loop: empty pipe + live writer → not ready.
        assert!(
            !read_target_ready(&target),
            "empty pipe with a writer is not ready"
        );
        let (sh2, tg2) = (Arc::clone(&shared), ReadTarget::Pipe(Arc::clone(&pipe)));
        let reader = std::thread::spawn(move || block_until(&sh2, || read_target_ready(&tg2)));

        // Let it park (one FUTEX_POLL chunk is 50 ms; this is well under it), then the
        // sibling `write` fills the buffer — the resume trigger.
        std::thread::sleep(Duration::from_millis(20));
        pipe.lock().unwrap().data.extend(b"payload".iter().copied());

        // The block returns ready (not exit-driven), and the data is intact for the read.
        assert!(
            reader.join().unwrap(),
            "the sibling write resumed the parked read"
        );
        assert!(read_target_ready(&target), "data is now ready");
        let mut b = pipe.lock().unwrap();
        let got: Vec<u8> = b.data.drain(..).collect();
        assert_eq!(
            &got, b"payload",
            "the reader resumes with exactly the written bytes"
        );
    }

    /// task-125: a pipe drained of data with its last writer closed is EOF, not a block —
    /// `read_target_ready` returns true so the driver completes the read as `0` (EOF)
    /// rather than parking forever. Mirrors the empty-pipe/no-writer inline path.
    #[test]
    fn drained_pipe_without_writer_is_ready_eof() {
        use crate::shim::PipeBuf;
        use std::collections::VecDeque;
        let pipe = Arc::new(Mutex::new(PipeBuf::with(VecDeque::new(), 0, 1)));
        let target = ReadTarget::Pipe(pipe);
        assert!(
            read_target_ready(&target),
            "a drained, writer-less pipe is EOF-ready, not a block"
        );
    }

    /// task-125 (process-exit-during-block): a reader parked on an empty pipe that never
    /// receives data observes `exited` and returns cleanly (`false` = exit, not ready) —
    /// no hang. Mirrors the `wait_released_by_process_exit` shape for the read path.
    #[test]
    fn blocking_read_released_by_process_exit() {
        use crate::shim::PipeBuf;
        use std::collections::VecDeque;

        let pipe = Arc::new(Mutex::new(PipeBuf::with(VecDeque::new(), 1, 1)));
        let shared = Arc::new(ThreadShared::new(Arc::new(MtClock::default())));
        let (sh2, tg2) = (Arc::clone(&shared), ReadTarget::Pipe(Arc::clone(&pipe)));
        // Nobody ever writes; only the process exit can end this park.
        let reader = std::thread::spawn(move || block_until(&sh2, || read_target_ready(&tg2)));
        std::thread::sleep(Duration::from_millis(20));
        shared.exited.store(true, Ordering::Relaxed);
        assert!(
            !reader.join().unwrap(),
            "process exit unparks the reader (false = exit, no hang)"
        );
    }
}
