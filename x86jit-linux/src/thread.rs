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
//! Current status: **P2.1 + P2.2 skeleton** — `ThreadShared` exists and the driver
//! runs a *single* worker thread through the real `shim.handle()` loop, to validate
//! the Send refactor + the `&Vm` migration + the lock discipline against the whole
//! single-process corpus before any concurrency (futex/clone land in P2.3+).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use x86jit_core::{Exit, Reg, Vcpu, Vm};

use crate::proc::{ProcError, ProcOutcome};
use crate::LinuxShim;

/// A guest thread returns to the driver periodically (this many blocks) even when it
/// isn't issuing syscalls, so it notices a sibling's `exit_group`.
const BUDGET: u64 = 50_000;

/// Process-wide thread state, held **outside** the shim mutex so a blocked thread
/// (futex wait, later epoll) never holds the shim lock. Self-synchronizing — every
/// field is atomic or its own lock.
pub struct ThreadShared {
    /// Per-address futex wake generation: a `FUTEX_WAIT`er sleeps until its address's
    /// generation advances (a `FUTEX_WAKE`). Wired in P2.3.
    pub futex: Mutex<HashMap<u64, u64>>,
    pub futex_cv: Condvar,
    /// Set when any thread runs `exit_group`: every vcpu loop stops at its next budget.
    pub exited: AtomicBool,
    /// The process exit code, published by the thread that runs `exit_group`.
    pub exit_code: AtomicU64,
    /// Monotonic thread-id source for `gettid` / `clone` child tids (P2.4/P2.5).
    pub next_tid: AtomicU64,
    /// Spawned worker join handles, drained on process exit. Populated by `clone` (P2.4).
    pub threads: Mutex<Vec<JoinHandle<()>>>,
}

impl ThreadShared {
    fn new(root_tid: u64) -> Self {
        ThreadShared {
            futex: Mutex::new(HashMap::new()),
            futex_cv: Condvar::new(),
            exited: AtomicBool::new(false),
            exit_code: AtomicU64::new(0),
            // Child tids start above the root pid/tid.
            next_tid: AtomicU64::new(root_tid + 1),
            threads: Mutex::new(Vec::new()),
        }
    }
}

/// Drive an already-loaded process as a threaded process to completion. The caller
/// (the OCI runner, or the deferred scheduler on escalation) has built `vm`, loaded
/// the program, set RIP/RSP on `cpu`, and configured `shim` — this only adds the
/// threaded execution model on top.
///
/// P2.2 skeleton: runs the single main thread. `clone(CLONE_VM)` spawning lands in P2.4.
pub fn run_threaded(vm: Vm, cpu: Vcpu, shim: LinuxShim) -> Result<ProcOutcome, ProcError> {
    let root_tid = shim.pid;
    let vm = Arc::new(vm);
    let shim = Arc::new(Mutex::new(shim));
    let shared = Arc::new(ThreadShared::new(root_tid));

    // Run the main thread on THIS thread; `clone` will spawn additional workers over
    // the same three Arcs (P2.4).
    run_vcpu(&vm, cpu, &shim, &shared)?;

    // Join every worker the process spawned (none yet in the skeleton).
    let handles: Vec<_> = std::mem::take(&mut *shared.threads.lock().unwrap());
    for h in handles {
        let _ = h.join();
    }

    let guard = shim.lock().unwrap();
    Ok(ProcOutcome {
        stdout: guard.stdout.clone(),
        exit_code: guard
            .exit_code
            .unwrap_or_else(|| shared.exit_code.load(Ordering::Relaxed) as i32),
    })
}

/// One guest thread's execution loop: run the vcpu, service each syscall under the
/// shim lock, stop when the process exits. A budget makes a compute-bound thread
/// return here periodically to observe `exited`.
fn run_vcpu(
    vm: &Arc<Vm>,
    mut cpu: Vcpu,
    shim: &Arc<Mutex<LinuxShim>>,
    shared: &Arc<ThreadShared>,
) -> Result<(), ProcError> {
    loop {
        if shared.exited.load(Ordering::Relaxed) {
            return Ok(());
        }
        match cpu.run(vm, Some(BUDGET)) {
            Exit::BudgetExhausted => continue,
            Exit::Syscall => {
                // Lock the shim only across the syscall itself; guest compute above is
                // lock-free. `handle` returns true when it wants the driver's attention.
                let yielded = {
                    let mut s = shim.lock().unwrap();
                    s.handle(&mut cpu, vm)
                };
                if yielded {
                    let s = shim.lock().unwrap();
                    if s.exit_code.is_some() {
                        // Skeleton: any exit ends the process. exit vs exit_group and
                        // per-thread exit land in P2.5.
                        return Ok(());
                    }
                    // A pending_* yield (fork/wait/exec/pipe-read): unsupported for a
                    // threaded process — surfaced as an error, never a host panic (P2.8).
                    return Err(ProcError::Trapped(
                        "threaded process used fork/wait/execve/blocking-pipe — unsupported (P2.8)"
                            .into(),
                    ));
                }
            }
            other => {
                return Err(ProcError::Trapped(format!(
                    "threaded process: {other:?} at rip={:#x}",
                    cpu.reg(Reg::Rip)
                )));
            }
        }
    }
}
