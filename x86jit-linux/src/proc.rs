//! Deferred-child process scheduler (OCI-4, oci-multiprocess-plan.md §5).
//!
//! The guest-agnostic core forks a VM ([`Vm::fork_with_backend`]) but knows nothing
//! about pids, fd inheritance, or reaping — that OS process model lives here, on the
//! embedder side of the boundary (spec §1/§4.1).
//!
//! Model: **deferred child, sequential, unbounded pipes.** `fork` snapshots the
//! parent into a child but does NOT run it; `wait4` runs a pending child to
//! completion (recursively — a child may fork too), then resumes the parent. This is
//! deterministic — children run in a fixed order — which the differential oracle
//! needs, and it handles the common shell pipeline (`echo | cat`). The cost: a
//! process that fills a pipe and blocks before its reader runs would deadlock, so
//! pipe buffers are unbounded (a writer never blocks). True concurrency and
//! full-pipe backpressure are out of scope for this rung (§2).

use std::collections::BTreeMap;

use x86jit_core::{Backend, Exit, Reg, Vcpu, Vm};

use crate::shim::ExecRequest;
use crate::LinuxShim;

/// `-ECHILD`: `wait4` with no children to reap.
const ECHILD: u64 = (-10i64) as u64;
/// `-EAGAIN`: `fork` can't be satisfied (host-backed Reserved memory the core can't
/// deep-copy) — fork's resource-exhaustion errno, which every runtime handles.
const EAGAIN: u64 = (-11i64) as u64;
/// pid handed to the root process; children get monotonically larger numbers.
const ROOT_PID: u64 = 1000;

/// A freshly-loaded process image for `execve`. The scheduler owns the process model
/// but not the ELF loader (that lives in the embedder's runner), so `execve` calls
/// back to a caller-supplied loader that returns this.
pub struct ExecImage {
    pub vm: Vm,
    pub entry: u64,
    pub rsp: u64,
    pub brk: u64,
    pub brk_limit: u64,
    pub mmap_base: u64,
    pub mmap_limit: u64,
}

/// Why a process tree stopped short of a clean exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcError {
    /// A guest trap the scheduler doesn't handle (unknown instruction, MMIO, …).
    Trapped(String),
    /// An `execve` whose image the loader couldn't produce.
    Exec(String),
}

impl std::fmt::Display for ProcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcError::Trapped(m) => write!(f, "guest trapped: {m}"),
            ProcError::Exec(m) => write!(f, "execve: {m}"),
        }
    }
}

/// Observable result of running a process tree: merged stdout (children first, in
/// completion order) and the root's exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcOutcome {
    pub stdout: Vec<u8>,
    /// Merged stderr (task-129) — a guest's fd-2 writes, so a failing run's diagnostics
    /// (a Go panic, caddy's boot errors) are observable instead of dropped.
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

/// How one process's [`run_process`](Scheduler::run_process) loop ended.
enum RunOutcome {
    /// The process exited cleanly with this code.
    Exited(i32),
    /// The process hit its first `clone(CLONE_VM)` — a thread, not a fork. It escalates
    /// to the threaded driver (task-126): the deferred model can't run shared-address-space
    /// threads. The clone was peeked but left un-serviced (RIP already past the `syscall`);
    /// the scheduler hands this whole `Process` to `run_threaded_escalated`, which services
    /// that one pending clone and drives the process threaded. Boxed: a `Process` (its
    /// vcpu carries the x87/vector register files) dwarfs the `Exited` variant, and
    /// escalation is a cold, once-per-process path.
    Escalate(Box<Process>),
}

/// One guest process: its VM, execution context, OS state, and any deferred children
/// not yet reaped.
pub struct Process {
    vm: Vm,
    cpu: Vcpu,
    shim: LinuxShim,
    pid: u64,
    /// Children created by `fork` but not yet run, keyed by pid. `wait4` drains them.
    pending: BTreeMap<u64, Process>,
    /// Children that have run to completion but not yet been reaped: pid → exit code.
    zombies: BTreeMap<u64, i32>,
}

type ExecLoader = Box<dyn Fn(&ExecRequest) -> Result<ExecImage, String>>;

/// Drives a process tree under the deferred-child model. Owns a backend factory (so
/// each forked child gets its own backend of the same engine as the root) and,
/// optionally, an `execve` image loader.
pub struct Scheduler {
    make_backend: Box<dyn Fn() -> Box<dyn Backend>>,
    exec_loader: Option<ExecLoader>,
    next_pid: u64,
}

impl Scheduler {
    /// A scheduler that can fork/wait but not `execve` (a guest `execve` becomes a
    /// [`ProcError::Trapped`]). Enough for the fork/pipe tests.
    pub fn new(make_backend: impl Fn() -> Box<dyn Backend> + 'static) -> Self {
        Scheduler {
            make_backend: Box::new(make_backend),
            exec_loader: None,
            next_pid: ROOT_PID,
        }
    }

    /// Install an `execve` image loader (the OCI runner supplies ELF loading). On a
    /// guest `execve` the scheduler reloads the process image in place, preserving
    /// the fd table and stdout (same process, new program).
    pub fn with_exec_loader(
        mut self,
        loader: impl Fn(&ExecRequest) -> Result<ExecImage, String> + 'static,
    ) -> Self {
        self.exec_loader = Some(Box::new(loader));
        self
    }

    /// Drive `vm`/`cpu`/`shim` as the root process to completion. The caller has
    /// already loaded the program and set RIP/RSP; the scheduler only adds the
    /// process model on top.
    pub fn run(
        &mut self,
        vm: Vm,
        cpu: Vcpu,
        mut shim: LinuxShim,
    ) -> Result<ProcOutcome, ProcError> {
        // The root reports the conventional init-of-the-tree pid; children get 1001+.
        shim.pid = ROOT_PID;
        shim.ppid = 0;
        let root = Process {
            vm,
            cpu,
            shim,
            pid: ROOT_PID,
            pending: BTreeMap::new(),
            zombies: BTreeMap::new(),
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        match self.run_process(root, &mut stdout, &mut stderr)? {
            RunOutcome::Exited(exit_code) => Ok(ProcOutcome {
                stdout,
                stderr,
                exit_code,
            }),
            // First `clone(CLONE_VM)`: the root goes threaded (task-126). The deferred
            // model can't run it, so hand the whole `(vm, cpu, shim)` to the threaded
            // driver, which services the one peeked-but-un-serviced clone and drives the
            // process to completion. The escalating process has no live children (a
            // pending-child process is refused escalation in `run_process`), so nothing
            // is orphaned. The threaded driver owns stdout/stderr from here, so we drop
            // what the deferred phase buffered on the *process* shim — but a pre-clone
            // print is flushed into `stdout`/`stderr` here first so it isn't lost.
            RunOutcome::Escalate(proc) => {
                let mut proc = *proc;
                stdout.append(&mut proc.shim.stdout);
                stderr.append(&mut proc.shim.stderr);
                let outcome = crate::thread::run_threaded_escalated(proc.vm, proc.cpu, proc.shim)?;
                stdout.extend_from_slice(&outcome.stdout);
                stderr.extend_from_slice(&outcome.stderr);
                Ok(ProcOutcome {
                    stdout,
                    stderr,
                    exit_code: outcome.exit_code,
                })
            }
        }
    }

    fn alloc_pid(&mut self) -> u64 {
        self.next_pid += 1;
        self.next_pid
    }

    /// Run one process until it exits, appending its stdout to `out` on exit — after
    /// its children, which complete during its `wait4`s (post-order). Returns its
    /// exit code.
    fn run_process(
        &mut self,
        mut proc: Process,
        out: &mut Vec<u8>,
        err: &mut Vec<u8>,
    ) -> Result<RunOutcome, ProcError> {
        loop {
            // `guarded_run` recovers a JIT guard-page SIGSEGV into Exit::UnmappedMemory
            // (doc-30, task-127) — the deferred single-vcpu process path gets it too.
            match crate::sigsegv::guarded_run(&mut proc.cpu, &proc.vm, None) {
                Exit::Syscall => {
                    // Peek for the deferred→threaded escalation trigger (task-126) BEFORE
                    // `handle()` services the syscall: a `clone(CLONE_VM)` is a
                    // shared-address-space thread the deferred model can't run. The core
                    // already advanced RIP past the `syscall`, and `Rax`/`Rdi` still hold
                    // the clone args, so `run_threaded_escalated` can service the clone as
                    // its first act. Two register reads + a branch, only on the syscall
                    // path — the single-threaded fast path is untouched.
                    if crate::thread::is_clone_vm(&proc.cpu) {
                        // A threaded process is one-directional (P2.8): it can't have
                        // deferred children waiting to be reaped, or the handoff would
                        // orphan them. In practice a threaded binary clones before it
                        // forks, so this never has children; guard it anyway.
                        if !proc.pending.is_empty() || !proc.zombies.is_empty() {
                            return Err(ProcError::Trapped(format!(
                                "process {}: clone(CLONE_VM) with {} pending / {} zombie \
                                 child(ren) — escalation would orphan them (P2.8)",
                                proc.pid,
                                proc.pending.len(),
                                proc.zombies.len()
                            )));
                        }
                        return Ok(RunOutcome::Escalate(Box::new(proc)));
                    }
                    if !proc.shim.handle(&mut proc.cpu, &proc.vm) {
                        continue; // ordinary syscall, serviced in-shim
                    }
                    // handle() yielded to the driver: fork, wait4, exec, or exit.
                    if proc.shim.pending_fork {
                        proc.shim.pending_fork = false;
                        match self.spawn_child(&proc) {
                            Some(child) => {
                                let pid = child.pid;
                                proc.pending.insert(pid, child);
                                proc.cpu.set_reg(Reg::Rax, pid); // parent gets the child pid
                            }
                            // Host-backed Reserved memory the core can't deep-copy →
                            // give the guest -EAGAIN (fork's resource errno, as the
                            // threaded path does) rather than aborting the host.
                            None => proc.cpu.set_reg(Reg::Rax, EAGAIN),
                        }
                        continue;
                    }
                    if let Some(req) = proc.shim.pending_wait.take() {
                        // Flush the parent's stdout-so-far BEFORE running its children,
                        // so bytes it printed before this `wait4` land ahead of the
                        // child output produced during the reap — real syscall order,
                        // not completion order (#11).
                        out.append(&mut proc.shim.stdout);
                        err.append(&mut proc.shim.stderr);
                        // Deferred model: run every not-yet-run child NOW, in fork
                        // (pid) order, so a pipeline's writer (forked first) runs
                        // before its reader — unbounded buffers then carry the data
                        // across. Completed children become zombies to be reaped.
                        self.reap_pending(&mut proc, out, err)?;
                        match take_zombie(&mut proc.zombies, req.pid) {
                            Some((cpid, code)) => {
                                if req.status_ptr != 0 {
                                    // WEXITSTATUS in bits 8..16 of the status word.
                                    let status = ((code as u32) & 0xff) << 8;
                                    let _ =
                                        proc.vm.write_bytes(req.status_ptr, &status.to_le_bytes());
                                }
                                proc.cpu.set_reg(Reg::Rax, cpid);
                            }
                            None => proc.cpu.set_reg(Reg::Rax, ECHILD),
                        }
                        continue;
                    }
                    if let Some(req) = proc.shim.pending_exec.take() {
                        self.exec_in_place(&mut proc, &req)?;
                        continue;
                    }
                    if let Some(pr) = proc.shim.pending_read.take() {
                        // Same ordering flush as `wait4` before running children (#11).
                        out.append(&mut proc.shim.stdout);
                        err.append(&mut proc.shim.stderr);
                        // A pipe read that would block: run pending writer children to
                        // fill the pipe (a `$(...)` substitution has the parent read a
                        // child's output), then complete the read.
                        self.reap_pending(&mut proc, out, err)?;
                        let ret = proc.shim.resume_read(&proc.vm, pr.fd, pr.buf, pr.len);
                        proc.cpu.set_reg(Reg::Rax, ret);
                        continue;
                    }
                    // A real exit: close fds (EOF for any peer), publish stdout, unwind.
                    let code = proc.shim.exit_code.unwrap_or(0);
                    proc.shim.close_all_fds();
                    out.extend_from_slice(&proc.shim.stdout);
                    err.extend_from_slice(&proc.shim.stderr);
                    return Ok(RunOutcome::Exited(code));
                }
                other => {
                    // Surface the trap loudly at the source (an unknown instruction
                    // prints its bytes) before returning it as an error (task-132).
                    crate::report_gap(&other);
                    return Err(ProcError::Trapped(format!(
                        "process {}: {other:?} at rip={:#x}",
                        proc.pid,
                        proc.cpu.reg(Reg::Rip)
                    )));
                }
            }
        }
    }

    /// Run every deferred child of `proc` to completion in fork (pid) order, moving
    /// each into the zombie table. A child may itself fork and reap grandchildren
    /// (handled by the recursion); newly-forked siblings appearing mid-loop are
    /// picked up because we re-check `pending` each iteration.
    fn reap_pending(
        &mut self,
        proc: &mut Process,
        out: &mut Vec<u8>,
        err: &mut Vec<u8>,
    ) -> Result<(), ProcError> {
        while let Some(&pid) = proc.pending.keys().next() {
            let child = proc.pending.remove(&pid).expect("pid just observed");
            match self.run_process(child, out, err)? {
                RunOutcome::Exited(code) => {
                    proc.zombies.insert(pid, code);
                }
                // A forked child that itself goes threaded (task-126): its parent is mid-
                // `wait4`/read reaping it. The child has no children of its own at its clone
                // point (the escalation guard in `run_process` ensures that), so it runs
                // threaded to completion here and becomes a zombie the parent reaps — the
                // same shape as a non-threaded child, just driven threaded.
                RunOutcome::Escalate(esc) => {
                    let esc = *esc;
                    let outcome = crate::thread::run_threaded_escalated(esc.vm, esc.cpu, esc.shim)?;
                    out.extend_from_slice(&outcome.stdout);
                    err.extend_from_slice(&outcome.stderr);
                    proc.zombies.insert(pid, outcome.exit_code);
                }
            }
        }
        Ok(())
    }

    /// Replace `proc`'s image with the `execve` target (same process: fd table and
    /// stdout persist; memory, registers, and brk/mmap reset). Pipes stay open across
    /// exec — that's how a shell's child inherits its redirected stdin/stdout.
    fn exec_in_place(&self, proc: &mut Process, req: &ExecRequest) -> Result<(), ProcError> {
        let load = self
            .exec_loader
            .as_ref()
            .ok_or_else(|| ProcError::Trapped("execve with no image loader installed".into()))?;
        let img = load(req).map_err(ProcError::Exec)?;
        proc.vm = img.vm;
        proc.cpu = proc.vm.new_vcpu();
        proc.cpu.set_reg(Reg::Rip, img.entry);
        proc.cpu.set_reg(Reg::Rsp, img.rsp);
        proc.shim.brk = img.brk;
        proc.shim.brk_limit = img.brk_limit;
        proc.shim.mmap_base = img.mmap_base;
        proc.shim.mmap_limit = img.mmap_limit;
        Ok(())
    }

    /// Snapshot `parent` into a deferred child: a forked VM with its own backend, a
    /// fresh vcpu carrying the parent's CPU state with RAX = 0 (the child's fork
    /// return), and a forked shim (inherited fd table).
    /// `None` when the parent's memory can't be forked by the core — a host-backed
    /// `Reserved` span (only the embedder can re-allocate it). The caller then returns
    /// the guest `-EAGAIN`, matching the threaded fork policy (deferred.md), instead of
    /// aborting the host (was a `deep_copy` panic).
    fn spawn_child(&mut self, parent: &Process) -> Option<Process> {
        let pid = self.alloc_pid();
        let child_vm = parent.vm.fork_with_backend((self.make_backend)())?;
        let mut child_cpu = child_vm.new_vcpu();
        child_cpu.cpu = parent.cpu.cpu.clone();
        child_cpu.set_reg(Reg::Rax, 0);
        // `fork()` already set the child's ppid to the parent; give it its real pid so
        // `getpid` in the child matches the pid the parent got back from `fork` (#10).
        let mut child_shim = parent.shim.fork();
        child_shim.pid = pid;
        // fork() seeded next_tid from the parent's pid; reseed to the child's pid+1 so that
        // if this child escalates on clone(CLONE_VM) (task-126) its first thread doesn't
        // collide with its own main-thread tid (== its pid).
        child_shim.reseed_next_tid(pid + 1);
        Some(Process {
            vm: child_vm,
            cpu: child_cpu,
            shim: child_shim,
            pid,
            pending: BTreeMap::new(),
            zombies: BTreeMap::new(),
        })
    }
}

/// Reap a completed child: the exact pid if `want > 0` (and present), else any (the
/// lowest pid). Returns `(pid, exit_code)`, or `None` when there's nothing to reap.
fn take_zombie(zombies: &mut BTreeMap<u64, i32>, want: i64) -> Option<(u64, i32)> {
    let key = if want > 0 {
        let k = want as u64;
        zombies.contains_key(&k).then_some(k)?
    } else {
        *zombies.keys().next()?
    };
    zombies.remove(&key).map(|code| (key, code))
}
