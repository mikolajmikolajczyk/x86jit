//! Real multithreaded guest program (M7, spec §11): a static-musl C program with
//! four pthreads, each incrementing a shared counter under a mutex 100 000 times.
//! The result is deterministic (400 000) only if *guest* threads, cross-thread
//! atomics, and the futex-backed mutex/join all work. Each guest thread runs on
//! its own host thread over one `Arc<Vm>` (shared memory + translation cache) —
//! `clone` spawns them, a real `futex` blocks/wakes them.
//!
//! This exercises the whole M7 stack end to end on a genuine program, on both
//! backends. (Weak-host TSO ordering, M7-T4, still needs an ARM host.)

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vcpu, Vm, VmConfig};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};

const FLAT: u64 = 0x200_0000; // 32 MiB
const HEAP_BASE: u64 = 0x60_0000;
const STACK_TOP: u64 = 0xf0_0000;
const MMAP_BASE: u64 = 0x100_0000; // thread stacks come from here

// clone(2) flags we honor.
const CLONE_SETTLS: u64 = 0x0008_0000;
const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
const CLONE_CHILD_SETTID: u64 = 0x0100_0000;

/// Process-wide state shared by every guest thread's host thread.
struct Shared {
    stdout: Mutex<Vec<u8>>,
    /// Per-address wake generation; a `futex` waiter sleeps until its address's
    /// generation advances (a `FUTEX_WAKE`).
    futex: Mutex<HashMap<u64, u64>>,
    futex_cv: Condvar,
    mmap_next: AtomicU64,
    mmap_end: u64,
    exited: AtomicBool,
    exit_code: AtomicU64,
    next_tid: AtomicU64,
    children: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    fn futex_wait(&self, vm: &Vm, addr: u64, val: u32) -> u64 {
        let mut g = self.futex.lock().unwrap();
        if read_u32(vm, addr) != val {
            return (-11i64) as u64; // -EAGAIN: value already changed
        }
        let gen = *g.entry(addr).or_insert(0);
        loop {
            if self.exited.load(Ordering::Relaxed) {
                return 0;
            }
            let (ng, _to) = self
                .futex_cv
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap();
            g = ng;
            if *g.get(&addr).unwrap_or(&0) != gen {
                return 0; // woken by FUTEX_WAKE on this address
            }
        }
    }

    fn futex_wake(&self, addr: u64, n: u64) -> u64 {
        let mut g = self.futex.lock().unwrap();
        *g.entry(addr).or_insert(0) += 1;
        self.futex_cv.notify_all(); // each waiter re-checks its own address
        n
    }
}

fn read_u32(vm: &Vm, addr: u64) -> u32 {
    vm.mem.read(addr, 4).unwrap_or(0) as u32
}

/// What a syscall did to the calling thread.
enum Step {
    Continue,
    ThreadExit,  // `exit` — this thread ends
    ProcessExit, // `exit_group` — the whole process ends
}

/// Run one vcpu until it exits, servicing syscalls; `clone` spawns more. On thread
/// exit, clear the child-tid and wake any joiner (the pthread_join protocol).
fn run_vcpu(vm: Arc<Vm>, mut cpu: Vcpu, shared: Arc<Shared>, clear_tid: u64) {
    'outer: loop {
        if shared.exited.load(Ordering::Relaxed) {
            break;
        }
        // A budget makes a compute-bound thread return here periodically to notice
        // process exit even when it isn't issuing syscalls.
        match cpu.run(&vm, Some(50_000)) {
            Exit::BudgetExhausted => continue,
            Exit::Syscall => match handle(&mut cpu, &vm, &shared) {
                Step::Continue => {}
                Step::ThreadExit => break 'outer,
                Step::ProcessExit => {
                    shared.exit_code.store(cpu.reg(Reg::Rdi), Ordering::Relaxed);
                    shared.exited.store(true, Ordering::Relaxed);
                    shared.futex_cv.notify_all(); // release every parked waiter
                    break 'outer;
                }
            },
            other => panic!("unexpected exit at rip={:#x}: {other:?}", cpu.reg(Reg::Rip)),
        }
    }
    // pthread exit: the kernel writes 0 to the CLONE_CHILD_CLEARTID address and
    // wakes a futex on it — that's how the joiner learns this thread finished.
    if clear_tid != 0 {
        let _ = vm.mem.write(clear_tid, 0, 4);
        shared.futex_wake(clear_tid, 1);
    }
}

fn handle(cpu: &mut Vcpu, vm: &Arc<Vm>, shared: &Arc<Shared>) -> Step {
    let nr = cpu.reg(Reg::Rax);
    match nr {
        1 => {
            // write(fd, buf, len)
            let (fd, buf, len) = (
                cpu.reg(Reg::Rdi),
                cpu.reg(Reg::Rsi),
                cpu.reg(Reg::Rdx) as usize,
            );
            let mut data = vec![0u8; len];
            vm.read_bytes(buf, &mut data).ok();
            if fd == 1 {
                shared.stdout.lock().unwrap().extend_from_slice(&data);
            }
            cpu.set_reg(Reg::Rax, len as u64);
        }
        202 => {
            // futex(uaddr, op, val, ...)
            let addr = cpu.reg(Reg::Rdi);
            let op = cpu.reg(Reg::Rsi) & 0x7f; // strip FUTEX_PRIVATE / _CLOCK_REALTIME
            let val = cpu.reg(Reg::Rdx) as u32;
            let ret = match op {
                0 => shared.futex_wait(vm, addr, val),    // FUTEX_WAIT
                1 => shared.futex_wake(addr, val as u64), // FUTEX_WAKE
                _ => 0,
            };
            cpu.set_reg(Reg::Rax, ret);
        }
        56 => {
            // clone(flags, stack, ptid, ctid, tls) — spawn a guest thread.
            let flags = cpu.reg(Reg::Rdi);
            let stack = cpu.reg(Reg::Rsi);
            let ptid = cpu.reg(Reg::Rdx);
            let ctid = cpu.reg(Reg::R10);
            let tls = cpu.reg(Reg::R8);
            let tid = shared.next_tid.fetch_add(1, Ordering::Relaxed);

            let mut child = cpu.cpu.clone();
            child.gpr[0] = 0; // child returns 0 from clone
            child.gpr[4] = stack; // RSP
            if flags & CLONE_SETTLS != 0 {
                child.fs_base = tls;
            }
            if flags & CLONE_PARENT_SETTID != 0 {
                let _ = vm.mem.write(ptid, tid, 4);
            }
            if flags & CLONE_CHILD_SETTID != 0 {
                let _ = vm.mem.write(ctid, tid, 4);
            }
            let clear = if flags & CLONE_CHILD_CLEARTID != 0 {
                ctid
            } else {
                0
            };

            let mut child_vcpu = vm.new_vcpu();
            child_vcpu.cpu = child;
            let (vm2, sh2) = (Arc::clone(vm), Arc::clone(shared));
            let h = std::thread::spawn(move || run_vcpu(vm2, child_vcpu, sh2, clear));
            shared.children.lock().unwrap().push(h);
            cpu.set_reg(Reg::Rax, tid); // parent gets the child tid
        }
        9 => {
            // mmap — anonymous bump from the shared arena (thread stacks).
            let len = cpu.reg(Reg::Rsi);
            let aligned = (len + 0xfff) & !0xfff;
            let addr = shared.mmap_next.fetch_add(aligned, Ordering::Relaxed);
            let ret = if addr + aligned <= shared.mmap_end {
                addr
            } else {
                (-12i64) as u64
            };
            cpu.set_reg(Reg::Rax, ret);
        }
        158 => {
            // arch_prctl(ARCH_SET_FS, addr)
            if cpu.reg(Reg::Rdi) == 0x1002 {
                cpu.set_reg(Reg::FsBase, cpu.reg(Reg::Rsi));
            }
            cpu.set_reg(Reg::Rax, 0);
        }
        218 => cpu.set_reg(Reg::Rax, 1000), // set_tid_address
        60 => return Step::ThreadExit,      // exit (this thread)
        231 => return Step::ProcessExit,    // exit_group
        // benign no-ops: munmap, mprotect, rt_sigprocmask, rt_sigaction,
        // set_robust_list, madvise, sigaltstack.
        11 | 10 | 14 | 13 | 273 | 28 | 131 => cpu.set_reg(Reg::Rax, 0),
        334 => cpu.set_reg(Reg::Rax, (-38i64) as u64), // rseq -> -ENOSYS
        other => panic!("unhandled syscall {other}"),
    }
    Step::Continue
}

fn run_threaded(backend: Box<dyn Backend>) -> Vec<u8> {
    run_threaded_cfg(backend, false)
}

fn run_threaded_cfg(backend: Box<dyn Backend>, tier_background: bool) -> Vec<u8> {
    let image = include_bytes!("../programs/pthreads.elf");
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
    // bg-tier BGT-4 (S5): background tier-up under real multi-vcpu concurrency — the
    // hot counter loop tiers up across threads, each completion drained/published
    // exactly once (the `done`/`tier_pending` locks serialize it). Off by default.
    if tier_background {
        vm.set_tier_up_after(Some(50));
        vm.set_tier_up_background(true);
    }
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

    let shared = Arc::new(Shared {
        stdout: Mutex::new(Vec::new()),
        futex: Mutex::new(HashMap::new()),
        futex_cv: Condvar::new(),
        mmap_next: AtomicU64::new(MMAP_BASE),
        mmap_end: FLAT - 0x1000,
        exited: AtomicBool::new(false),
        exit_code: AtomicU64::new(0),
        next_tid: AtomicU64::new(1001),
        children: Mutex::new(Vec::new()),
    });

    // Run the main thread on this thread; clone() spawns the rest.
    run_vcpu(Arc::new(vm), cpu, Arc::clone(&shared), 0);

    // Join every spawned host thread (all should have exited by now).
    let handles: Vec<_> = shared.children.lock().unwrap().drain(..).collect();
    for h in handles {
        let _ = h.join();
    }
    let out = shared.stdout.lock().unwrap().clone();
    out
}

fn reference() -> Vec<u8> {
    x86jit_tests::reference::reference(b"400000\n", || {
        std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/pthreads.elf"
        ))
        .output()
        .expect("run native pthreads")
        .stdout
    })
}

#[test]
fn pthreads_counter_interp() {
    assert_eq!(
        run_threaded(Box::new(InterpreterBackend)),
        reference(),
        "interpreter"
    );
}

#[test]
fn pthreads_counter_jit() {
    assert_eq!(
        run_threaded(Box::new(JitBackend::new())),
        reference(),
        "JIT"
    );
}

/// bg-tier BGT-4 (S5): the same four-thread counter under background tier-up. Real
/// concurrent vcpus over one `Arc<Vm>` drain and publish completions; the result must
/// still be exactly 400000, proving no completion is lost or double-applied.
#[test]
fn pthreads_counter_jit_background() {
    assert_eq!(
        run_threaded_cfg(Box::new(JitBackend::new()), true),
        reference(),
        "JIT background tier-up"
    );
}
