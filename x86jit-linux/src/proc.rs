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

use crate::LinuxShim;

/// `-ECHILD`: `wait4` with no children to reap.
const ECHILD: u64 = (-10i64) as u64;
/// pid handed to the root process; children get monotonically larger numbers.
const ROOT_PID: u64 = 1000;

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
}

/// Drives a process tree under the deferred-child model. Owns a backend factory so
/// each forked child gets its own backend of the same engine as the root.
pub struct Scheduler {
    make_backend: Box<dyn Fn() -> Box<dyn Backend>>,
    next_pid: u64,
}

impl Scheduler {
    pub fn new(make_backend: impl Fn() -> Box<dyn Backend> + 'static) -> Self {
        Scheduler {
            make_backend: Box::new(make_backend),
            next_pid: ROOT_PID,
        }
    }

    /// Drive `vm`/`cpu`/`shim` as the root process to completion. The caller has
    /// already loaded the program and set RIP/RSP; the scheduler only adds the
    /// process model on top.
    pub fn run(&mut self, vm: Vm, cpu: Vcpu, shim: LinuxShim) -> ProcOutcome {
        let root = Process {
            vm,
            cpu,
            shim,
            pid: ROOT_PID,
            pending: BTreeMap::new(),
        };
        let mut stdout = Vec::new();
        let exit_code = self.run_process(root, &mut stdout);
        ProcOutcome { stdout, exit_code }
    }

    fn alloc_pid(&mut self) -> u64 {
        self.next_pid += 1;
        self.next_pid
    }

    /// Run one process until it exits, appending its stdout to `out` on exit — after
    /// its children, which complete during its `wait4`s (post-order). Returns its
    /// exit code.
    fn run_process(&mut self, mut proc: Process, out: &mut Vec<u8>) -> i32 {
        loop {
            match proc.cpu.run(&proc.vm, None) {
                Exit::Syscall => {
                    if !proc.shim.handle(&mut proc.cpu, &mut proc.vm) {
                        continue; // ordinary syscall, serviced in-shim
                    }
                    // handle() yielded to the driver: fork, wait4, exit, or exec.
                    if proc.shim.pending_fork {
                        proc.shim.pending_fork = false;
                        let child = self.spawn_child(&proc);
                        let pid = child.pid;
                        proc.pending.insert(pid, child);
                        proc.cpu.set_reg(Reg::Rax, pid); // parent gets the child pid
                        continue;
                    }
                    if let Some(req) = proc.shim.pending_wait.take() {
                        match pick_child(&mut proc.pending, req.pid) {
                            Some(child) => {
                                let cpid = child.pid;
                                let code = self.run_process(child, out);
                                if req.status_ptr != 0 {
                                    // Normal-exit status word: WEXITSTATUS in bits 8..16.
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
                    if proc.shim.pending_exec.is_some() {
                        // Image reload lives in the OCI runner (it owns the ELF
                        // loader); this scheduler doesn't do exec-in-child yet.
                        panic!("execve inside the process scheduler is unsupported (OCI runner rung)");
                    }
                    // A real exit: publish this process's stdout, then unwind.
                    let code = proc.shim.exit_code.unwrap_or(0);
                    out.extend_from_slice(&proc.shim.stdout);
                    return code;
                }
                other => panic!("process {}: unexpected exit {other:?}", proc.pid),
            }
        }
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
        }
    }
}

/// Pick a child to reap: the exact pid if `want > 0` (and present), else any (the
/// lowest pid). Returns `None` when there's nothing to wait for.
fn pick_child(pending: &mut BTreeMap<u64, Process>, want: i64) -> Option<Process> {
    let key = if want > 0 {
        let k = want as u64;
        pending.contains_key(&k).then_some(k)?
    } else {
        *pending.keys().next()?
    };
    pending.remove(&key)
}
