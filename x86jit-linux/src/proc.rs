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
    pub exit_code: i32,
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
    pub fn run(&mut self, vm: Vm, cpu: Vcpu, shim: LinuxShim) -> Result<ProcOutcome, ProcError> {
        let root = Process {
            vm,
            cpu,
            shim,
            pid: ROOT_PID,
            pending: BTreeMap::new(),
            zombies: BTreeMap::new(),
        };
        let mut stdout = Vec::new();
        let exit_code = self.run_process(root, &mut stdout)?;
        Ok(ProcOutcome { stdout, exit_code })
    }

    fn alloc_pid(&mut self) -> u64 {
        self.next_pid += 1;
        self.next_pid
    }

    /// Run one process until it exits, appending its stdout to `out` on exit — after
    /// its children, which complete during its `wait4`s (post-order). Returns its
    /// exit code.
    fn run_process(&mut self, mut proc: Process, out: &mut Vec<u8>) -> Result<i32, ProcError> {
        loop {
            match proc.cpu.run(&proc.vm, None) {
                Exit::Syscall => {
                    if !proc.shim.handle(&mut proc.cpu, &mut proc.vm) {
                        continue; // ordinary syscall, serviced in-shim
                    }
                    // handle() yielded to the driver: fork, wait4, exec, or exit.
                    if proc.shim.pending_fork {
                        proc.shim.pending_fork = false;
                        let child = self.spawn_child(&proc);
                        let pid = child.pid;
                        proc.pending.insert(pid, child);
                        proc.cpu.set_reg(Reg::Rax, pid); // parent gets the child pid
                        continue;
                    }
                    if let Some(req) = proc.shim.pending_wait.take() {
                        // Deferred model: run every not-yet-run child NOW, in fork
                        // (pid) order, so a pipeline's writer (forked first) runs
                        // before its reader — unbounded buffers then carry the data
                        // across. Completed children become zombies to be reaped.
                        self.reap_pending(&mut proc, out)?;
                        match take_zombie(&mut proc.zombies, req.pid) {
                            Some((cpid, code)) => {
                                if req.status_ptr != 0 {
                                    // WEXITSTATUS in bits 8..16 of the status word.
                                    let status = ((code as u32) & 0xff) << 8;
                                    let _ = proc
                                        .vm
                                        .write_bytes(req.status_ptr, &status.to_le_bytes());
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
                        // A pipe read that would block: run pending writer children to
                        // fill the pipe (a `$(...)` substitution has the parent read a
                        // child's output), then complete the read.
                        self.reap_pending(&mut proc, out)?;
                        let ret = proc.shim.resume_read(&mut proc.vm, pr.fd, pr.buf, pr.len);
                        proc.cpu.set_reg(Reg::Rax, ret);
                        continue;
                    }
                    // A real exit: close fds (EOF for any peer), publish stdout, unwind.
                    let code = proc.shim.exit_code.unwrap_or(0);
                    proc.shim.close_all_fds();
                    out.extend_from_slice(&proc.shim.stdout);
                    return Ok(code);
                }
                other => {
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
    fn reap_pending(&mut self, proc: &mut Process, out: &mut Vec<u8>) -> Result<(), ProcError> {
        while let Some(&pid) = proc.pending.keys().next() {
            let child = proc.pending.remove(&pid).expect("pid just observed");
            let code = self.run_process(child, out)?;
            proc.zombies.insert(pid, code);
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
    fn spawn_child(&mut self, parent: &Process) -> Process {
        let pid = self.alloc_pid();
        let child_vm = parent.vm.fork_with_backend((self.make_backend)());
        let mut child_cpu = child_vm.new_vcpu();
        child_cpu.cpu = parent.cpu.cpu.clone();
        child_cpu.set_reg(Reg::Rax, 0);
        Process {
            vm: child_vm,
            cpu: child_cpu,
            shim: parent.shim.fork(),
            pid,
            pending: BTreeMap::new(),
            zombies: BTreeMap::new(),
        }
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
