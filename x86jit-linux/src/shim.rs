//! Linux x86-64 syscall shim (testing.md §9, spec §1/§4.1). The core never
//! emulates an OS (§1); this is the embedder that reacts to `Exit::Syscall`. It
//! backs both the differential test suite and the OCI image runner.
//!
//! Convention: number in RAX, args in RDI/RSI/RDX/R10/R8/R9, return in RAX. RIP
//! already points past the `syscall` (the engine's convention), so the driver
//! just calls `run()` again to resume.
//!
//! Most syscalls are modeled in-process (stdout capture, a bump `brk`). A few —
//! `open`/`read`/`close` — are *passed through* to the host kernel so a real
//! program can hash a real file (testing.md §12, the macro oracle). Passthrough
//! is off by default and, when enabled, restricted to read-only opens of an
//! explicit path allowlist: a test forwarding guest file I/O to the host is a
//! deliberate, bounded capability, not an ambient one.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use x86jit_core::{CpuMode, CpuState, Reg, Vcpu, Vm};

/// What a syscall did to the calling guest thread, for the threaded driver
/// ([`crate::thread`]). The single-process loop uses [`LinuxShim::handle`] (a
/// yield-bool); this is its multithread-aware sibling that surfaces the blocking and
/// lifecycle operations the driver must service **outside** the shim lock. Keeping
/// them out of the shim is what enforces the lock order (shim → futex, never the
/// reverse): the shim decodes the syscall, the driver drops the shim guard and then
/// blocks on `ThreadShared`.
pub enum SyscallOutcome {
    /// Fully serviced under the shim lock (`Rax` is set). Keep running this thread.
    Continue,
    /// `futex(FUTEX_WAIT, uaddr, val, timeout)`: the driver drops the shim lock, blocks
    /// on `ThreadShared`, then writes `Rax` (0 woken / -EAGAIN mismatch / -ETIMEDOUT).
    FutexWait {
        uaddr: u64,
        val: u32,
        /// Relative wait bound from the guest's `timespec`, or `None` for an
        /// indefinite wait. A `FUTEX_WAIT_BITSET` *absolute* deadline is converted to
        /// a relative bound (against the shared virtual clock) before it lands here, so
        /// the driver only ever sees a relative timeout (task-121).
        timeout: Option<Duration>,
        /// The waiter's bitmask (task-121). Plain `FUTEX_WAIT` is match-any
        /// (`0xffff_ffff`); `FUTEX_WAIT_BITSET` carries the guest's `val3` (R9). A
        /// `FUTEX_WAKE_BITSET` only releases a waiter whose stored bitmask ANDs nonzero
        /// with the waker's.
        bitmask: u32,
    },
    /// `futex(FUTEX_WAKE, uaddr, count)`: the driver wakes up to `count` waiters on
    /// the address and writes `Rax`.
    FutexWake {
        uaddr: u64,
        count: u64,
        /// The waker's bitmask (task-121). Plain `FUTEX_WAKE` is match-any
        /// (`0xffff_ffff`); `FUTEX_WAKE_BITSET` carries the guest's `val3` (R9). Only
        /// waiters whose stored bitmask intersects this are released.
        bitmask: u32,
    },
    /// `clone(CLONE_VM)`: spawn a sibling thread. The shim has already built the child
    /// `CpuState` (RAX=0, RSP, TLS), performed the PARENT/CHILD_SETTID writes, and set
    /// the parent's `Rax` to `child_tid`; the driver only does `new_vcpu()`, assigns the
    /// state, and spawns `run_vcpu` over the shared Arcs (P2.4).
    Spawn {
        /// Boxed to keep `SyscallOutcome` small — `CpuState` (vector + x87 register
        /// files) is the enum's heaviest payload and clone is a cold path.
        child_cpu: Box<CpuState>,
        child_tid: u64,
        /// `CLONE_CHILD_CLEARTID` address to hand the child's `ThreadCtx` (0 = none).
        clear_tid: u64,
    },
    /// `exit(2)`: only the calling thread ends (P2.5). The driver runs the clear_tid
    /// handshake and, if this was the last live thread, publishes `code` as the process
    /// status.
    ThreadExit(i32),
    /// `nanosleep`/`clock_nanosleep` in mt mode: the driver sleeps (interruptibly — in
    /// chunks that observe process exit) after the shim guard drops, so a sleeper never
    /// stalls a sibling's syscalls. The shim already set `Rax = 0` (P2.6).
    Sleep(Duration),
    /// `sched_yield`: the driver yields the host thread after the guard drops. `Rax = 0`
    /// already set (P2.6).
    Yield,
    /// A blocking `epoll_pwait` (go-caddy P4): the driver runs the real host `epoll_wait`
    /// in `FUTEX_POLL`-sized chunks after the guard drops (so a parked netpoller thread
    /// never holds the shim lock), writes the ready events, and sets `Rax`. The `Arc`
    /// keeps the host epoll fd alive across the block even if a sibling closes it.
    EpollWait {
        epfd: Arc<OwnedFd>,
        events_ptr: u64,
        maxevents: usize,
        /// `None` = infinite (`timeout_ms < 0`); the driver caps each host wait at
        /// `FUTEX_POLL` so it observes process exit.
        timeout: Option<Duration>,
    },
    /// A blocking `read`/`readv` on a pipe or host socket whose data isn't ready yet
    /// (task-125): the shim already proved it *would* block (an empty pipe with a live
    /// writer, or a host fd `poll`ing not-readable), so the driver parks outside the shim
    /// lock — in `FUTEX_POLL`-sized chunks that observe process exit — until data arrives
    /// (or EOF), then completes the read and writes `Rax`. Mirrors [`EpollWait`]: a
    /// blocking outcome carrying the read target so it stays alive across the block (a
    /// pipe's `Arc<Mutex<PipeBuf>>`, a socket's `Arc<OwnedFd>`) even if a sibling closes
    /// the guest fd.
    ///
    /// [`EpollWait`]: SyscallOutcome::EpollWait
    BlockingRead {
        target: ReadTarget,
        buf: u64,
        len: usize,
    },
    /// A blocking `accept`/`accept4` on a host listen socket with no pending connection
    /// (task-125): the driver `poll`s the listen fd in `FUTEX_POLL` chunks outside the
    /// shim lock, does the real `accept4` once a peer connects, then **re-takes the shim
    /// lock** to install the accepted socket in `fd_table` (fd allocation is shim state).
    /// The `Arc<OwnedFd>` keeps the listen fd alive across the block.
    BlockingAccept {
        listen: Arc<OwnedFd>,
        addr_ptr: u64,
        addrlen_ptr: u64,
        flags: libc::c_int,
    },
    /// A blocking `recvfrom`/`recvmsg` on a blocking-mode host socket with no data ready
    /// (task-233 — the `recv` analogue of [`BlockingRead`]). The shim already proved it
    /// *would* block (a blocking-mode socket that `poll`s not-readable), so the driver parks
    /// outside the shim lock — in `FUTEX_POLL`-sized chunks that observe process exit — until
    /// data arrives (or the peer hangs up), then re-takes the shim lock and completes the recv
    /// under it (readable + locked ⇒ won't block), reusing the exact inline writeback. The
    /// `Arc<OwnedFd>` keeps the socket alive across the block even if a sibling closes the
    /// guest fd; the args are boxed to keep `SyscallOutcome` small (like `Spawn`'s
    /// `Box<CpuState>`) since recv is a cold path relative to the enum's hot variants.
    ///
    /// [`BlockingRead`]: SyscallOutcome::BlockingRead
    BlockingRecv(Box<BlockingRecv>),
    /// `exit_group(code)`: the whole process ends with this code.
    ProcessExit(i32),
    /// A blocking or multi-process operation with no meaningful errno for a threaded
    /// process — `execve` (would kill all siblings and replace the image), `wait4`, or a
    /// blocking pipe read. The driver turns this into a `ProcError` naming `what`, never
    /// a host panic (P2.8). (`fork` is *not* here: it gets a guest-visible `-EAGAIN`
    /// instead — faking an execve errno would silently corrupt a run, but EAGAIN is
    /// fork's real, handled failure.)
    Unsupported { what: &'static str },
}

/// Which recv flavor a [`SyscallOutcome::BlockingRecv`] must re-run after the block (task-233).
/// The two share the same park machinery but different args + writeback: `Recvfrom` needs the
/// buffer/len and the peer-address out-params; `Recvmsg` needs the guest `msghdr` pointer (its
/// iovecs/control buffer are read from guest memory at completion time).
pub enum RecvKind {
    /// `recvfrom(fd, buf, len, flags, src_addr, addrlen*)`.
    Recvfrom {
        buf: u64,
        len: usize,
        /// `src_addr` out-param (0 = don't write the peer address).
        src: u64,
        /// `addrlen` in/out out-param.
        addrlen_ptr: u64,
    },
    /// `recvmsg(fd, msghdr*, flags)`.
    Recvmsg {
        /// Guest pointer to the `struct msghdr` (iovecs + control buffer read from it).
        msgp: u64,
    },
}

/// The parked-`recv` payload behind [`SyscallOutcome::BlockingRecv`] (task-233). Boxed inside
/// the outcome to keep `SyscallOutcome` small. Carries the host socket `Arc` (kept alive
/// across the block), the `flags`, and the flavor-specific args the completion re-runs with.
pub struct BlockingRecv {
    /// The host socket, held by value so it outlives the block even if a sibling closes the
    /// guest fd (mirrors [`ReadTarget::Host`]).
    pub fd: Arc<OwnedFd>,
    pub flags: libc::c_int,
    pub kind: RecvKind,
}

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_STAT: u64 = 4;
const SYS_FSTAT: u64 = 5;
const SYS_LSTAT: u64 = 6;
const SYS_LSEEK: u64 = 8;
const SYS_PIPE: u64 = 22;
const SYS_PIPE2: u64 = 293;
const SYS_CLONE: u64 = 56;
const SYS_FORK: u64 = 57;
const SYS_VFORK: u64 = 58;
const SYS_WAIT4: u64 = 61;
const SYS_DUP: u64 = 32;
const SYS_DUP2: u64 = 33;
const SYS_PREAD64: u64 = 17;
const SYS_PWRITE64: u64 = 18;
const SYS_FSYNC: u64 = 74;
const SYS_FDATASYNC: u64 = 75;
const SYS_FTRUNCATE: u64 = 77;
const SYS_UNLINK: u64 = 87;
const SYS_MKDIR: u64 = 83;
const SYS_MKDIRAT: u64 = 258;
const SYS_SYSINFO: u64 = 99;
const SYS_RENAME: u64 = 82;
const SYS_RENAMEAT: u64 = 264;
const SYS_RENAMEAT2: u64 = 316;
const SYS_CHMOD: u64 = 90;
const SYS_FCHMOD: u64 = 91;
const SYS_CHOWN: u64 = 92;
const SYS_FCHOWN: u64 = 93;
const SYS_UNLINKAT: u64 = 263;
const SYS_SET_ROBUST_LIST: u64 = 273;
const SYS_GET_ROBUST_LIST: u64 = 274;
/// `sizeof(struct robust_list_head)` — `{ list_head *next; long futex_offset; list_head
/// *pending; }` = 3×8 bytes. `set_robust_list` rejects any other `len` (-EINVAL),
/// exactly like the kernel (task-122).
const ROBUST_LIST_HEAD_SIZE: u64 = 24;
const SYS_PRLIMIT64: u64 = 302;
const SYS_GETRANDOM: u64 = 318;
const SYS_RSEQ: u64 = 334;
const SYS_FUTEX: u64 = 202;
const SYS_NEWFSTATAT: u64 = 262;
const SYS_POLL: u64 = 7;
const SYS_STATFS: u64 = 137;
const SYS_FSTATFS: u64 = 138;
const SYS_TGKILL: u64 = 234;
const SYS_PRCTL: u64 = 157;
const SYS_SCHED_GETAFFINITY: u64 = 204;
const SYS_CHDIR: u64 = 80;
// Socket family (go-caddy-plan.md Phase 0): forwarded to real host fds so a guest
// server binds a host-visible port. Numbers are the x86-64 Linux table.
const SYS_SOCKET: u64 = 41;
const SYS_CONNECT: u64 = 42;
const SYS_ACCEPT: u64 = 43;
const SYS_SENDTO: u64 = 44;
const SYS_RECVFROM: u64 = 45;
const SYS_SENDMSG: u64 = 46;
const SYS_RECVMSG: u64 = 47;
const SYS_SELECT: u64 = 23;
const SYS_PSELECT6: u64 = 270;
const SYS_SHUTDOWN: u64 = 48;
const SYS_BIND: u64 = 49;
const SYS_LISTEN: u64 = 50;
const SYS_GETSOCKNAME: u64 = 51;
const SYS_GETPEERNAME: u64 = 52;
const SYS_SETSOCKOPT: u64 = 54;
const SYS_GETSOCKOPT: u64 = 55;
const SYS_ACCEPT4: u64 = 288;

const ENOENT: u64 = (-2i64) as u64;
const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;
const SYS_BRK: u64 = 12;
const SYS_RT_SIGACTION: u64 = 13;
const SYS_RT_SIGPROCMASK: u64 = 14;
const SYS_MADVISE: u64 = 28;
const SYS_SIGALTSTACK: u64 = 131;
const SYS_EPOLL_WAIT: u64 = 232;
const SYS_EPOLL_CTL: u64 = 233;
const SYS_EPOLL_PWAIT: u64 = 281;
const SYS_EPOLL_CREATE1: u64 = 291;
const SYS_EVENTFD2: u64 = 290;
const SYS_IOCTL: u64 = 16;
const SYS_SCHED_YIELD: u64 = 24;
const SYS_READV: u64 = 19;
const SYS_WRITEV: u64 = 20;
const SYS_ACCESS: u64 = 21;
const SYS_GETPID: u64 = 39;
const SYS_GETPPID: u64 = 110;
const SYS_FCNTL: u64 = 72;
const SYS_GETCWD: u64 = 79;
const SYS_READLINK: u64 = 89;
const SYS_UNAME: u64 = 63;
const SYS_READLINKAT: u64 = 267;
const SYS_GETTID: u64 = 186;
const SYS_GETDENTS64: u64 = 217;
const SYS_NANOSLEEP: u64 = 35;
const SYS_TIME: u64 = 201;
const SYS_GETTIMEOFDAY: u64 = 96;
const SYS_CLOCK_GETTIME: u64 = 228;
const SYS_CLOCK_NANOSLEEP: u64 = 230;

/// Wall-clock epoch (seconds) the virtual clock counts up from (§ shim time, #13).
const CLOCK_BASE_SEC: i64 = 1_700_000_000;
/// Nanoseconds the virtual clock advances on each clock read — enough that a
/// deadline loop terminates quickly while staying deterministic.
const CLOCK_TICK_NS: u64 = 1_000_000; // 1 ms
/// mt-mode per-read quantum (VCLK, decision-6). Smaller than the single-threaded
/// `CLOCK_TICK_NS`: Go's runtime reads the clock heavily, so a 1 ms tick would
/// inflate perceived time ~20×. 10 µs approximates the interpreter's measured
/// read pacing (~45 µs), keeping virtual time close to real interpreter time while
/// staying backend/load-invariant. Tunable — the eager go_http leg is the gate.
const MT_CLOCK_TICK_NS: u64 = 100;

/// Rate-controlled virtual monotonic clock for mt mode (decision-6). The value is
/// **virtual** — it advances with guest progress (a per-read quantum plus
/// credit-on-expiry for real waits), never with host wall-time — while all blocking
/// stays real host blocking. Shared across a threaded process's vcpus: the shim
/// ticks it on clock reads (under the shim lock); the driver credits it on expired
/// waits (outside the lock) — hence the atomic. `Relaxed` suffices: it is a single
/// atomic whose `fetch_add`/`fetch_max` share one per-location total modification
/// order, and no other data is published through it — that order *is* the
/// monotonicity guarantee.
#[derive(Debug, Default)]
pub struct MtClock(AtomicU64);

impl MtClock {
    /// Advance by `quantum` and return the new value (a guest-visible clock read).
    pub fn tick(&self, quantum: u64) -> u64 {
        self.0.fetch_add(quantum, Ordering::Relaxed) + quantum
    }

    /// Read without advancing (the driver samples this before a real wait).
    pub fn peek(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// Credit a completed wait: monotone, and concurrent sleepers overlap like real
    /// time instead of summing (`fetch_max`, not `fetch_add`). The idle-path
    /// companion to [`try_advance_from`](Self::try_advance_from).
    pub fn advance_to(&self, target_ns: u64) {
        self.0.fetch_max(target_ns, Ordering::Relaxed);
    }

    /// Credit an expired wait **only if the clock has not moved since `entry`** was
    /// peeked — the discrete-event "warp virtual time to the next event only when the
    /// process is idle" rule (decision-6 M3). A `compare_exchange`: it succeeds
    /// (advancing to `target`) exactly when the process was time-silent for the whole
    /// wait — no other guest thread ticked a read or credited — and fails (a no-op)
    /// when concurrent reads already carried virtual time forward. This is what keeps a
    /// free-running periodic timer (Go's sysmon, a `time.Tick` loop) from ratcheting the
    /// clock at host wall-rate on a busy process: its credits are defeated by the
    /// workers' reads, so it fires on read-metered virtual time instead. Returns whether
    /// it advanced. Monotone: `target > entry`, and success requires the value still
    /// equal `entry`.
    pub fn try_advance_from(&self, entry: u64, target: u64) -> bool {
        self.0
            .compare_exchange(entry, target, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    /// Seed the clock (at the mt flip, from the single-threaded `clock_ns`) so the
    /// value never jumps backward across the switch.
    pub fn seed(&self, ns: u64) {
        self.0.store(ns, Ordering::Relaxed);
    }
}
const SYS_GETUID: u64 = 102;
const SYS_GETGID: u64 = 104;
const SYS_SETUID: u64 = 105;
const SYS_SETGID: u64 = 106;
const SYS_GETEUID: u64 = 107;
const SYS_GETEGID: u64 = 108;
const SYS_ARCH_PRCTL: u64 = 158;
const SYS_SET_TID_ADDRESS: u64 = 218;
const SYS_EXIT: u64 = 60;
const SYS_OPENAT: u64 = 257;
const SYS_EXECVE: u64 = 59;
const SYS_EXIT_GROUP: u64 = 231;
const ARCH_SET_FS: u64 = 0x1002;

// i386 (`int 0x80`) syscall numbers — a *different* table from the x86-64 one above
// (exit=1 not 60, write=4 not 1, brk=45 not 12, mmap2=192, …). Only the numbers a
// static musl/glibc i386 hello actually issues are named; everything else is
// rejected loudly with its number (§17.7, TASK-93 grow-on-demand).
const SYS32_EXIT: u64 = 1;
const SYS32_READ: u64 = 3;
const SYS32_WRITE: u64 = 4;
const SYS32_OPEN: u64 = 5;
const SYS32_CLOSE: u64 = 6;
const SYS32_BRK: u64 = 45;
const SYS32_READLINK: u64 = 85;
const SYS32_MUNMAP: u64 = 91;
const SYS32_MPROTECT: u64 = 125;
const SYS32_WRITEV: u64 = 146;
const SYS32_UNAME: u64 = 122;
const SYS32_MMAP2: u64 = 192;
const SYS32_SET_THREAD_AREA: u64 = 243;
const SYS32_EXIT_GROUP: u64 = 252;
const SYS32_SET_TID_ADDRESS: u64 = 258;
const SYS32_READLINKAT: u64 = 305;
const SYS32_GETRANDOM: u64 = 355;

const ENOTTY: u64 = (-25i64) as u64;
const ENOMEM: u64 = (-12i64) as u64;

const O_ACCMODE: u64 = 0o3;
const O_RDONLY: u64 = 0;
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;

/// `-EACCES` / `-ENOENT` etc. as the kernel returns them: a small negative in RAX.
const EACCES: u64 = (-13i64) as u64;
const EAGAIN: u64 = (-11i64) as u64;
const EBADF: u64 = (-9i64) as u64;
const EFAULT: u64 = (-14i64) as u64;
const ENOSYS: u64 = (-38i64) as u64;
const EINVAL: u64 = (-22i64) as u64;
const EPERM: u64 = (-1i64) as u64;

/// Deterministic responses for syscalls beyond the built-ins, keyed by number
/// (testing.md §9). Keeps whole-program tests reproducible when a program issues
/// a syscall whose real effect we don't model — return a scripted value.
#[derive(Default)]
pub struct ScriptedSyscalls {
    pub responses: Vec<(u64, u64)>,
}

impl ScriptedSyscalls {
    fn get(&self, nr: u64) -> Option<u64> {
        self.responses
            .iter()
            .find(|(n, _)| *n == nr)
            .map(|(_, r)| *r)
    }
}

/// One guest file descriptor. Every fd — the standard streams included — routes
/// through the fd table so `dup2`/`pipe` can redirect them uniformly (a
/// `dup2(pipe_write, 1)` must make `write(1)` go to the pipe, not stdout). Files
/// live behind `Arc<Mutex<..>>` so a `dup`/`dup2` alias shares the seek offset
/// (POSIX). Single-threaded deferred model — `Rc`, not `Arc`.
enum Fd {
    Stdin,
    Stdout,
    Stderr,
    File(Arc<Mutex<OpenEntry>>),
    PipeRead(Arc<Mutex<PipeBuf>>),
    PipeWrite(Arc<Mutex<PipeBuf>>),
    /// A real host socket (listen or connected). `read`/`write`/`close` and the
    /// socket syscalls forward to this host fd, so the guest binds a host-visible
    /// port and the host can connect to it (go-caddy-plan.md Phase 0). Shared behind
    /// `Rc` so `dup`/fork alias the same underlying socket (the last drop closes it).
    Socket(Arc<OwnedFd>),
    /// A real host `epoll` instance (go-caddy P4). Go's netpoller registers its
    /// sockets here; `epoll_ctl`/`epoll_pwait` forward to the real kernel epoll, so
    /// readiness — including edge-triggered semantics — is the kernel's, not ours.
    Epoll(Arc<OwnedFd>),
    /// A real host `eventfd` (go-caddy P4). Go's netpoller adds one to its epoll set
    /// and writes it from `netpollBreak` to interrupt a blocked `epoll_pwait`.
    Event(Arc<OwnedFd>),
}

/// A pipe's shared byte buffer. **Unbounded** (a writer never blocks): the deferred,
/// single-threaded process model runs a writer to completion before its reader, so
/// pipe backpressure never arises (documented limitation, oci-multiprocess-plan.md
/// §2). `writers`/`readers` count the open ends so a read past the last writer sees
/// EOF (a drained buffer already reads as EOF here).
pub struct PipeBuf {
    pub(crate) data: VecDeque<u8>,
    pub(crate) writers: usize,
    readers: usize,
    /// O_NONBLOCK on the read end (task-232): a self-pipe / event-loop guest sets this via
    /// `pipe2(O_NONBLOCK)` or `fcntl(F_SETFL)` and expects an immediate `-EAGAIN` on an
    /// empty pipe with a live writer, never a park. Honored by `read_would_block` (serve
    /// inline, don't yield) and `do_read` (empty+writers → `-EAGAIN`, not EOF). The flag
    /// rides on the shared buffer so a `dup`'d read end observes the same mode.
    pub(crate) nonblocking: bool,
}

impl PipeBuf {
    /// A pipe buffer with `writers` open write ends and `readers` open read ends and the
    /// given initial bytes (blocking mode). Used by the pipe setup paths and the task-125
    /// blocking-read tests to build a target without going through the full `pipe(2)`
    /// syscall.
    #[cfg(test)]
    pub(crate) fn with(data: VecDeque<u8>, writers: usize, readers: usize) -> Self {
        PipeBuf {
            data,
            writers,
            readers,
            nonblocking: false,
        }
    }
}

/// Read-only host filesystem passthrough (testing.md §12). Disabled unless an
/// allowlist is installed; only exact paths on it may be opened, and only
/// `O_RDONLY`. Guest fds we hand out start at 3 and index `fd_table` — a guest
/// can only `read`/`close` a descriptor this shim itself opened, never an
/// arbitrary host fd.
struct FsPassthrough {
    allow: Vec<PathBuf>,
    /// `(path suffix, host file)`: any guest open of a path ending in the suffix
    /// (and not a `glibc-hwcaps` variant) is served from the host file. Lets a
    /// dynamic loader find e.g. `libc.so.6` from a checked-in fixture regardless of
    /// the machine-specific absolute path baked into the binary.
    serve: Vec<(Vec<u8>, PathBuf)>,
    /// Absolute host directory prefixes under which any read-only open is passed
    /// through. Lets an interpreter read its whole stdlib tree (dozens of files)
    /// without an entry per file. Still read-only, still bounded to the subtree.
    dirs: Vec<PathBuf>,
    /// Absolute host directory prefixes under which a **writable** open
    /// (`O_RDWR`/`O_WRONLY`, with `O_CREAT`/`O_TRUNC`) is passed through — a real
    /// on-disk file. Scoped to a test's temp dir so a guest can't touch anything
    /// else. Backs a file-DB program (sqlite's `<db>`, its `-journal`/`-wal`).
    write_dirs: Vec<PathBuf>,
    /// Rootfs mode (OCI images): when set, every guest path resolves *inside* this
    /// directory (chroot-like), read and write. Takes precedence over the
    /// allowlist/dir/serve mechanisms, which stay for the differential test suite.
    root: Option<PathBuf>,
    /// Every open descriptor, standard streams included. Seeded 0→Stdin, 1→Stdout,
    /// 2→Stderr; host opens take the lowest free fd ≥ 3.
    fd_table: BTreeMap<u64, Fd>,
}

impl Default for FsPassthrough {
    fn default() -> Self {
        let mut fd_table = BTreeMap::new();
        fd_table.insert(0, Fd::Stdin);
        fd_table.insert(1, Fd::Stdout);
        fd_table.insert(2, Fd::Stderr);
        FsPassthrough {
            allow: Vec::new(),
            serve: Vec::new(),
            dirs: Vec::new(),
            write_dirs: Vec::new(),
            root: None,
            fd_table,
        }
    }
}

/// A passthrough descriptor: either a regular file, or a directory whose entries
/// are snapshotted at `open` time and streamed by `getdents64` (an interpreter's
/// import machinery lists directories to find modules).
enum OpenEntry {
    File(File),
    Dir(Box<DirState>), // boxed: much larger than the File variant
}

struct DirState {
    meta: std::fs::Metadata,
    entries: Vec<DirEnt>,
    pos: usize,
}

struct DirEnt {
    name: Vec<u8>,
    ino: u64,
    dtype: u8,
}

impl OpenEntry {
    fn as_file(&self) -> Option<&File> {
        match self {
            OpenEntry::File(f) => Some(f),
            _ => None,
        }
    }
    fn as_file_mut(&mut self) -> Option<&mut File> {
        match self {
            OpenEntry::File(f) => Some(f),
            _ => None,
        }
    }
    fn metadata(&self) -> Option<std::fs::Metadata> {
        match self {
            OpenEntry::File(f) => f.metadata().ok(),
            OpenEntry::Dir(d) => Some(d.meta.clone()),
        }
    }
}

impl FsPassthrough {
    /// The host-backed entry behind `fd`, if it's a `File` (not a standard stream).
    /// Returns an `Rc` clone so callers can borrow it independently.
    fn file(&self, fd: u64) -> Option<Arc<Mutex<OpenEntry>>> {
        match self.fd_table.get(&fd) {
            Some(Fd::File(rc)) => Some(rc.clone()),
            _ => None,
        }
    }

    /// The pipe buffer behind `fd` if it's the read end.
    fn pipe_read(&self, fd: u64) -> Option<Arc<Mutex<PipeBuf>>> {
        match self.fd_table.get(&fd) {
            Some(Fd::PipeRead(rc)) => Some(rc.clone()),
            _ => None,
        }
    }

    /// The raw host fd behind `fd` if it's a socket. A plain `i32` (Copy), so the
    /// caller can drop the table borrow before an `unsafe` libc call.
    fn socket_fd(&self, fd: u64) -> Option<i32> {
        match self.fd_table.get(&fd) {
            Some(Fd::Socket(rc)) => Some(rc.as_raw_fd()),
            _ => None,
        }
    }

    /// The raw host fd behind any host-backed descriptor — a socket, an `eventfd`, or an
    /// `epoll` instance (go-caddy P4). `epoll_ctl` accepts all three (epoll fds can
    /// nest); read/write use it for sockets and eventfds. A plain `i32` (Copy) so the
    /// caller can drop the table borrow before the `unsafe` libc call.
    fn host_io_fd(&self, fd: u64) -> Option<i32> {
        match self.fd_table.get(&fd) {
            Some(Fd::Socket(rc) | Fd::Event(rc) | Fd::Epoll(rc)) => Some(rc.as_raw_fd()),
            _ => None,
        }
    }

    /// Run `op` on the host `File` behind `fd`, returning its `u64` syscall result;
    /// yields `-EBADF` if `fd` isn't an open passthrough file (task-173). Collapses the
    /// repeated `match file(fd) { Some(rc) => match lock.as_file_mut() { Some(f) => …,
    /// None => EBADF }, None => EBADF }` at every file-op syscall arm into one call.
    fn with_file(&self, fd: u64, op: impl FnOnce(&mut File) -> u64) -> u64 {
        match self.file(fd) {
            Some(rc) => match rc.lock().unwrap().as_file_mut() {
                Some(f) => op(f),
                None => EBADF,
            },
            None => EBADF,
        }
    }

    /// Would a `read(fd)` block? True only for a *blocking-mode* pipe read end whose buffer
    /// is empty while a writer is still open — the case the scheduler resolves by running a
    /// pending writer child. An empty pipe with no writers is EOF (returns 0), not a block;
    /// a nonblocking read end never blocks (task-232) — `do_read` returns `-EAGAIN` inline.
    fn pipe_would_block(&self, fd: u64) -> bool {
        match self.fd_table.get(&fd) {
            Some(Fd::PipeRead(rc)) => {
                let b = rc.lock().unwrap();
                b.data.is_empty() && b.writers > 0 && !b.nonblocking
            }
            _ => false,
        }
    }

    /// Lowest free fd ≥ 3 (dup2 may plant entries at arbitrary numbers, so scan).
    fn alloc_fd(&self) -> u64 {
        self.alloc_fd_from(3)
    }

    /// Lowest free fd ≥ `min` (`F_DUPFD` hands out the lowest free fd at or above a
    /// caller-chosen floor).
    fn alloc_fd_from(&self, min: u64) -> u64 {
        let mut fd = min;
        while self.fd_table.contains_key(&fd) {
            fd += 1;
        }
        fd
    }

    /// Map a guest path to the host file it may read: an exact allowlist entry, a
    /// suffix redirect (never a `glibc-hwcaps` probe), or a path under a permitted
    /// directory prefix. `..` components are rejected so a prefix can't be escaped.
    fn resolve_host(&self, path: &[u8]) -> Option<PathBuf> {
        if let Some(root) = &self.root {
            return rootfs_join(root, path);
        }
        if self
            .allow
            .iter()
            .any(|p| p.as_os_str().as_encoded_bytes() == path)
        {
            return Some(PathBuf::from(String::from_utf8_lossy(path).into_owned()));
        }
        if !contains(path, b"glibc-hwcaps") {
            if let Some((_, host)) = self
                .serve
                .iter()
                .find(|(s, _)| path.ends_with(s.as_slice()))
            {
                return Some(host.clone());
            }
        }
        if contains(path, b"/..") {
            return None; // no directory-prefix escape
        }
        let p = PathBuf::from(String::from_utf8_lossy(path).into_owned());
        if self.dirs.iter().any(|d| p.starts_with(d)) {
            return Some(p);
        }
        None
    }

    /// Map a guest path to a **writable** host file: it must lie under a permitted
    /// write-dir prefix (and contain no `..` escape). Identity mapping — the guest
    /// passes the real absolute host path the test set up.
    fn resolve_host_write(&self, path: &[u8]) -> Option<PathBuf> {
        if let Some(root) = &self.root {
            return rootfs_join(root, path);
        }
        if contains(path, b"/..") {
            return None;
        }
        let p = PathBuf::from(String::from_utf8_lossy(path).into_owned());
        self.write_dirs
            .iter()
            .any(|d| p.starts_with(d))
            .then_some(p)
    }
}

/// Resolve a guest path *inside* `root` (chroot-like, OCI rootfs mode) — a userspace
/// `openat2(RESOLVE_IN_ROOT)`. Walks the path one component at a time and **follows
/// symlinks within the root**: `..` never climbs above `root`, and an absolute
/// symlink target (`/leak -> /etc/passwd`) is re-rooted at `root`, not the host `/`.
/// The returned host path is fully symlink-resolved and provably under `root`, so a
/// subsequent `File::open`/`metadata` cannot traverse out of the rootfs. Returns
/// `None` on a symlink-loop budget exhaustion (an escape is clamped, not rejected).
///
/// Untrusted OCI images ship attacker-controlled symlinks; without this the OS would
/// follow `/leak -> /etc/passwd` straight to the host file (read *and* write). Residual
/// TOCTOU (a symlink swapped between resolve and open) is out of scope for a per-run
/// temp rootfs; `openat2` would close even that.
/// Resolve a guest path inside an OCI `rootfs`, symlink-safe and escape-proof —
/// the public entry point for the embedder's ELF loader (which resolves the
/// entrypoint and `PT_INTERP` paths outside the shim). See [`rootfs_join`].
pub fn resolve_in_rootfs(root: &std::path::Path, guest_path: &[u8]) -> Option<PathBuf> {
    rootfs_join(root, guest_path)
}

fn rootfs_join(root: &std::path::Path, path: &[u8]) -> Option<PathBuf> {
    // Split a byte path on '/', dropping empty and "." components.
    fn parts(p: &[u8]) -> Vec<Vec<u8>> {
        p.split(|&b| b == b'/')
            .filter(|c| !c.is_empty() && c != b".")
            .map(|c| c.to_vec())
            .collect()
    }

    let mut cur = root.to_path_buf(); // always within root
                                      // Work list of components still to resolve (a stack we consume from the front).
    let mut pending: std::collections::VecDeque<Vec<u8>> = parts(path).into();
    let mut symlink_budget = 40i32;

    while let Some(comp) = pending.pop_front() {
        if comp == b".." {
            if cur != root {
                cur.pop();
            }
            continue;
        }
        let cand = cur.join(std::ffi::OsStr::from_bytes(&comp));
        match std::fs::symlink_metadata(&cand) {
            Ok(m) if m.file_type().is_symlink() => {
                symlink_budget -= 1;
                if symlink_budget < 0 {
                    return None; // symlink loop
                }
                let Ok(target) = std::fs::read_link(&cand) else {
                    return None;
                };
                let tbytes = target.as_os_str().as_encoded_bytes();
                if tbytes.first() == Some(&b'/') {
                    cur = root.to_path_buf(); // absolute target re-roots at the rootfs
                }
                // Resolve the target's components before the rest of the path.
                for c in parts(tbytes).into_iter().rev() {
                    pending.push_front(c);
                }
            }
            // Regular entry, or doesn't exist yet (let the caller's open return ENOENT).
            _ => cur = cand,
        }
    }
    Some(cur)
}

/// Captures a program's observable output: bytes written to stdout/stderr and the
/// exit code. Compare these (not raw state) for a deterministic whole-program
/// oracle (testing.md §12.3).
#[derive(Default)]
pub struct LinuxShim {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub scripted: ScriptedSyscalls,
    /// Program break for a minimal `brk` allocator (0 = unset). `brk_limit` caps it.
    pub brk: u64,
    pub brk_limit: u64,
    /// Bump pointer + cap for an anonymous `mmap` arena (0 = unset).
    pub mmap_base: u64,
    pub mmap_limit: u64,
    /// Highest address the bump has ever reached (the dirty high-water mark). Memory at
    /// or above this was never written, so a fresh bump allocation there is implicitly
    /// zero; a bump allocation *below* it (into space a top-of-bump `munmap` rolled back
    /// over) may hold stale bytes and must be re-zeroed so a reused mmap reads back zero
    /// like a real anonymous map (task-124).
    mmap_high: u64,
    /// Live anonymous arena spans (`addr → aligned_len`), so `munmap` knows the exact
    /// extent it is freeing. Only bump-arena allocations enter here; `MAP_FIXED` and
    /// file-backed maps are untracked (task-124).
    mmap_live: BTreeMap<u64, u64>,
    /// Freed anonymous spans (`addr → aligned_len`) available for reuse, kept coalesced.
    /// `mmap` first-fits this before advancing `mmap_base`, so a thread-churning guest
    /// that `munmap`s joined stacks doesn't monotonically exhaust the arena. A span freed
    /// at the top of the bump rolls `mmap_base` back instead of landing here (task-124).
    mmap_free: BTreeMap<u64, u64>,
    /// Bytes the guest reads from fd 0 (stdin). A file-DB CLI reads its script here.
    pub stdin: Vec<u8>,
    stdin_pos: usize,
    /// Path `readlinkat`/`readlink` of `/proc/self/exe` reports — Go's `os.Executable`
    /// reads it at startup (caddy, task-162). Empty (the default) → `-ENOENT`, letting
    /// the guest fall back; the harness sets it to the entrypoint's argv[0].
    pub exe_path: Vec<u8>,
    fs: FsPassthrough,
    /// Syscall numbers we've already warned about (log-once for the gap reporter).
    gap_syscalls: std::collections::HashSet<u64>,
    /// Set by a guest `execve`: the embedder replaces the process image with this
    /// program and re-runs. `handle` returns `true` (leaves `run()`), and the
    /// driver checks this to distinguish exec from exit (OCI-4).
    pub pending_exec: Option<ExecRequest>,
    /// Set by a guest `fork`/`clone`/`vfork`: `handle` returns `true` and the
    /// process scheduler ([`crate::proc`]) snapshots the VM into a deferred child,
    /// then RESUMES this parent (unlike exec/exit, which leave for good).
    pub pending_fork: bool,
    /// Set by a guest `wait4`: the scheduler runs a pending child to completion and
    /// writes its status back before resuming the parent.
    pub pending_wait: Option<WaitRequest>,
    /// Set by a `read` on a pipe that would block (empty buffer, a writer still
    /// open): the scheduler runs pending writer children to fill the pipe, then
    /// completes the read. This is the "pull" that makes a parent-as-reader command
    /// substitution (`$(...)`) work in the deferred model.
    pub pending_read: Option<PendingRead>,
    /// This process's pid / parent pid, wired by the scheduler ([`crate::proc`]) on
    /// fork so `getpid`/`getppid`/`gettid` report the real value the parent got back
    /// from `fork`/`wait4`, not a constant. `new()` seeds the root pid; a standalone
    /// shim (no scheduler) keeps that so single-process tests are unaffected.
    pub pid: u64,
    pub ppid: u64,
    /// Reused byte buffer for syscall payloads (write/writev/pwrite copy guest bytes
    /// through it), so an I/O syscall doesn't malloc+free a fresh `Vec` each time.
    scratch: Vec<u8>,
    /// Monotonic virtual clock in nanoseconds since process start (§ shim time).
    /// Every clock read ticks it a fixed quantum and `nanosleep` advances it by the
    /// requested duration, so a guest deadline/`nanosleep` loop makes progress —
    /// still fully deterministic (a function of the syscall sequence), unlike the old
    /// frozen epoch that spun such loops forever (#13).
    clock_ns: u64,
    /// Monotonic thread-id source for `clone(CLONE_VM)` child tids, seeded `pid + 1`.
    /// Lives here (not in `ThreadShared`) because `handle_mt`'s clone arm — which needs
    /// it for the child tid, the parent's `Rax`, and the SETTID writes — runs under the
    /// shim lock and can't reach `ThreadShared` (P2.4).
    next_tid: u64,
    /// Set the first time a `clone(CLONE_VM)` is accepted: from then on the process has
    /// real sibling threads ("mt mode"), which flips the clock domain (P2.6). The
    /// single-threaded corpus never trips this, so its deterministic clock is preserved.
    pub threaded: bool,
    /// The shared virtual monotonic clock for mt mode (VCLK, decision-6). Seeded from
    /// `clock_ns` at the flip so time never jumps backward, then `now_ns` ticks it on
    /// every read and the threaded driver credits it on expired waits (via
    /// `ThreadShared`). Per-process: a fork gets a fresh `Arc`.
    mt_clock: Arc<MtClock>,
    /// Signal dispositions (`rt_sigaction`), one 32-byte `kernel_sigaction` per signal
    /// 1..=64. Process-wide by POSIX. We store and read them back (Go's `initsig` queries
    /// every signal to build `fwdSig`) but never deliver — P3 is "no delivery".
    sigactions: Vec<[u8; 32]>,
    /// The single-threaded `sigaltstack` / `rt_sigprocmask` state. In a threaded process
    /// these live per-thread in [`ThreadCtx`]; here they serve the single-vcpu path.
    altstack: SigAltStack,
    sigmask: u64,
    /// `getrandom`/`AT_RANDOM` entropy source (task-128). Default [`EntropyMode::Deterministic`].
    pub entropy: EntropyMode,
    /// splitmix64 state for the deterministic entropy stream (seeded in `new`).
    rng_state: u64,
}

/// Entropy source for `getrandom` / `AT_RANDOM` (task-128).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EntropyMode {
    /// A fixed-seed PRNG: byte streams reproduce exactly across runs (the differential
    /// corpus needs this — interp and JIT are separate shims and must agree). Varied
    /// bytes (unlike the old constant `0x42`), so crypto that needs distinct randomness
    /// still functions, just deterministically.
    #[default]
    Deterministic,
    /// Real host entropy (`/dev/urandom`). MANDATORY for serving TLS: a constant/seeded
    /// stream under HTTPS means predictable keys — a security-grade bug.
    HostEntropy,
}

/// A guest `sigaltstack` (`stack_t { void *ss_sp; int ss_flags; size_t ss_size }`).
/// Recorded and read back but never used for delivery (P3). Per-thread — Go installs a
/// separate signal stack for every M.
#[derive(Clone, Copy)]
pub struct SigAltStack {
    pub sp: u64,
    pub size: u64,
    pub flags: i32,
}

/// `SS_DISABLE`: the alt stack is not installed. The initial state, and what a
/// `sigaltstack(nil, &old)` query must read back (not uninitialized guest garbage).
const SS_DISABLE: i32 = 2;

impl Default for SigAltStack {
    fn default() -> Self {
        SigAltStack {
            sp: 0,
            size: 0,
            flags: SS_DISABLE,
        }
    }
}

/// Per-guest-thread state owned by the driver's `run_vcpu` frame (never shared) and
/// passed `&mut` into [`LinuxShim::handle_mt`], so the per-thread syscalls
/// (`gettid` / `set_tid_address` / `exit`) answer per thread without a shared registry
/// (P2.5). Its natural owner is the host thread running the vcpu — same lifetime, no
/// synchronization.
pub struct ThreadCtx {
    /// This thread's tid: the root pid for the main thread, the clone-assigned tid for a
    /// worker.
    pub tid: u64,
    /// `CLONE_CHILD_CLEARTID` / `set_tid_address` address. On thread exit the driver
    /// writes 0 here and futex-wakes it — the handshake a `pthread_join` waits on. 0 =
    /// none.
    pub clear_tid: u64,
    /// This thread's `sigaltstack` — per-thread (Go installs one per M). Recorded and
    /// read back, never delivered (P3).
    pub altstack: SigAltStack,
    /// This thread's blocked-signal mask (`rt_sigprocmask`) — per-thread, read back but
    /// with no delivery effect (P3).
    pub sigmask: u64,
    /// This thread's robust-futex list head (`set_robust_list`, task-122), or 0 = none.
    /// The kernel walks this on thread exit and, for each held mutex, sets
    /// `FUTEX_OWNER_DIED` in the futex word and wakes a waiter (so a surviving locker
    /// gets `EOWNERDEAD` instead of deadlocking on a dead owner). Per-thread, exactly
    /// like `clear_tid`. `get_robust_list` reads it back.
    pub robust_list_head: u64,
    /// The `len` argument `set_robust_list` was called with (the size of the
    /// `robust_list_head` struct). Recorded so `get_robust_list` can return it; the walk
    /// itself uses the kernel's fixed field offsets, not this length.
    pub robust_list_len: u64,
}

/// Where a threaded blocking `read` ([`SyscallOutcome::BlockingRead`]) draws its bytes
/// from, held by value so the target outlives the block even if a sibling closes the guest
/// fd (task-125). A pipe is the in-process [`PipeBuf`] (a sibling `write` fills it); a
/// socket/eventfd is a real host fd the driver `read`s once it polls readable.
pub enum ReadTarget {
    /// An in-process pipe read end: the driver waits for a sibling `write` to fill the
    /// buffer (data ready), or the last writer to close (drained → EOF).
    Pipe(Arc<Mutex<PipeBuf>>),
    /// A real host fd (socket/eventfd): the driver `poll`s it readable, then `read`s.
    Host(Arc<OwnedFd>),
}

/// A `read` parked because its pipe would block — see [`LinuxShim::pending_read`].
#[derive(Debug, Clone, Copy)]
pub struct PendingRead {
    pub fd: u64,
    pub buf: u64,
    pub len: usize,
}

/// A guest `wait4(pid, status, options, rusage)` for the scheduler to fulfill.
#[derive(Debug, Clone, Copy)]
pub struct WaitRequest {
    /// The `pid` argument: `> 0` waits for that child, `<= 0` for any child.
    pub pid: i64,
    /// Where to write the exit status (`int*`); 0 = the guest passed NULL.
    pub status_ptr: u64,
}

/// A guest `execve(path, argv, envp)` request for the embedder to fulfill by
/// loading a fresh process image.
#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub path: Vec<u8>,
    pub argv: Vec<Vec<u8>>,
    pub envp: Vec<Vec<u8>>,
}

impl LinuxShim {
    pub fn new() -> Self {
        // Seed the conventional root pid so a standalone shim (used directly, without
        // the process scheduler) reports 1000 as before; the scheduler overwrites it.
        Self {
            pid: 1000,
            next_tid: 1001,
            // One (zeroed = SIG_DFL) disposition slot per signal 1..=64.
            sigactions: vec![[0u8; 32]; 64],
            // Fixed splitmix64 seed → the deterministic entropy stream is identical run
            // to run (interp and JIT shims must agree, task-128).
            rng_state: 0x9E37_79B9_7F4A_7C15,
            ..Self::default()
        }
    }

    /// Select the entropy source for `getrandom`/`AT_RANDOM` (task-128). `HostEntropy`
    /// is required before serving TLS; `Deterministic` (default) keeps the differential
    /// corpus reproducible.
    pub fn set_entropy(&mut self, mode: EntropyMode) {
        self.entropy = mode;
    }

    /// Fill `self.scratch` with `len` bytes of entropy per [`EntropyMode`] (task-128).
    /// Returns `false` (caller returns `-EFAULT`/errno) if the length can't be reserved.
    #[must_use]
    fn fill_scratch_entropy(&mut self, len: usize) -> bool {
        if !self.try_resize_scratch(len) {
            return false;
        }
        match self.entropy {
            EntropyMode::Deterministic => {
                let mut i = 0;
                while i < self.scratch.len() {
                    // splitmix64.
                    self.rng_state = self.rng_state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                    let mut z = self.rng_state;
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                    z ^= z >> 31;
                    let bytes = z.to_le_bytes();
                    let n = (self.scratch.len() - i).min(8);
                    self.scratch[i..i + n].copy_from_slice(&bytes[..n]);
                    i += n;
                }
            }
            EntropyMode::HostEntropy => {
                use std::io::Read;
                // /dev/urandom is the Linux entropy source; a short read leaves zeros,
                // which is safe-but-degraded (never happens in practice for these sizes).
                if let Ok(mut f) = File::open("/dev/urandom") {
                    let _ = f.read_exact(&mut self.scratch);
                }
            }
        }
        true
    }

    /// The shared virtual monotonic clock (VCLK, decision-6), for the threaded driver
    /// to clone into `ThreadShared` so the driver can credit it on expired waits.
    pub(crate) fn mt_clock(&self) -> Arc<MtClock> {
        Arc::clone(&self.mt_clock)
    }

    /// Copy `len` guest bytes at `addr` into the reused `scratch` buffer (no
    /// per-syscall allocation); callers then read `&self.scratch`. Returns `false`
    /// (rather than panicking the host) when the guest range is unmapped or straddles a
    /// mapping edge, or the length is bogus, so the caller can return `-EFAULT`. Every
    /// syscall arm routes buffer copies through this or [`try_resize_scratch`] — the
    /// "no host panic from guest input" rule (§ hardening).
    #[must_use]
    fn try_fill_scratch(&mut self, vm: &Vm, addr: u64, len: usize) -> bool {
        // try_reserve so a bogus guest length can't abort the host on allocation.
        self.scratch.clear();
        if self.scratch.try_reserve(len).is_err() {
            return false;
        }
        self.scratch.resize(len, 0);
        vm.read_bytes(addr, &mut self.scratch).is_ok()
    }

    /// Grow `scratch` to `len` zeroed bytes for a host read, without aborting the host
    /// on a bogus guest length (`try_reserve`, not `resize`). `false` → caller returns
    /// an errno instead of panicking.
    #[must_use]
    fn try_resize_scratch(&mut self, len: usize) -> bool {
        self.scratch.clear();
        if self.scratch.try_reserve(len).is_err() {
            return false;
        }
        self.scratch.resize(len, 0);
        true
    }

    /// `madvise(MADV_DONTNEED)` a guest range `[addr, addr+len)` (task-131). Two goals,
    /// both preserved here:
    ///
    /// 1. **Correctness (SACRED, task-161):** after this the guest range MUST read back as
    ///    zero. Linux guarantees anonymous pages fault back zero after `MADV_DONTNEED`, and
    ///    Go's scavenger returns spans with `needzero == 0` trusting it; skipping the zero
    ///    corrupts the heap.
    /// 2. **RSS release:** merely zeroing the guest bytes leaves the host physical pages
    ///    resident, so a long-running Go server's scavenger never actually shrinks RSS. For
    ///    a host-mapped (`MAP_NORESERVE`) region we `madvise(MADV_DONTNEED)` the host backing
    ///    pages too, releasing them to the OS — and since they refault as zero, that single
    ///    host call *also* satisfies goal 1 for the covered pages (no explicit rewrite).
    ///
    /// Page alignment: `madvise` requires page-aligned addresses. We align the start **up**
    /// and the end **down** to `HOST_PAGE`, giving the fully-covered inner page range, and
    /// host-madvise only that — never spilling onto a page the guest didn't fully ask about.
    /// The partial edge bytes (below the first full page, above the last full page) are
    /// zeroed via the explicit `write_bytes` path, which also covers the Vec-backed backing
    /// (no host mapping to madvise) and any range that escapes a mapped region (best-effort,
    /// like the pre-131 arm — a bad guest length is silently not-zeroed, never a host abort).
    fn madvise_dontneed(&mut self, vm: &Vm, addr: u64, len: u64) {
        const HOST_PAGE: u64 = 4096;
        let Some(end) = addr.checked_add(len) else {
            return; // overflow: bogus guest range, best-effort no-op (never abort)
        };
        // Inner fully-covered page range: start rounded up, end rounded down. Rounding
        // `addr` up can itself overflow `u64` (a top-page `addr` near u64::MAX) even when
        // `addr + len` did not — `checked_mul` so a guest can't abort the host that way;
        // `None` means there is no full inner page, so treat the inner range as empty and
        // fall through to the whole-range zero (harden #1: guest input never crashes the host).
        let inner_start = match addr.div_ceil(HOST_PAGE).checked_mul(HOST_PAGE) {
            Some(s) => s,
            None => end,
        };
        let inner_end = (end / HOST_PAGE) * HOST_PAGE;

        // Host-madvise the inner pages iff non-empty AND backed by a real host mapping
        // (RAM). `host_ram_ptr` returns None for a Vec backing or a range escaping RAM.
        let mut madvised = false;
        if inner_start < inner_end {
            let inner_len = (inner_end - inner_start) as usize;
            if let Some(host_ptr) = vm.mem.host_ram_ptr(inner_start, inner_len) {
                // SAFETY: `host_ptr`/`inner_len` name a page-aligned sub-range wholly inside
                // the host `MAP_NORESERVE` mapping backing guest RAM (validated by
                // `host_ram_ptr`); `MADV_DONTNEED` on anonymous NORESERVE pages is defined
                // and makes them refault as zero. A negative return (e.g. EINVAL on a
                // non-anonymous edge) is ignored — we fall through to the zero fallback,
                // which still holds the read-back-zero postcondition.
                let rc = unsafe {
                    libc::madvise(
                        host_ptr as *mut libc::c_void,
                        inner_len,
                        libc::MADV_DONTNEED,
                    )
                };
                madvised = rc == 0;
            }
        }

        if madvised {
            // The inner pages were released and refault as zero. Zero only the partial edge
            // bytes the host madvise didn't cover: `[addr, inner_start)` and
            // `[inner_end, end)`. Both are guest sub-ranges; `write_bytes` is best-effort.
            self.zero_range(vm, addr, inner_start);
            self.zero_range(vm, inner_end, end);
        } else {
            // No host mapping (Vec backing), empty inner range, or the madvise failed:
            // fall back to zeroing the whole guest range, preserving the postcondition.
            self.zero_range(vm, addr, end);
        }
    }

    /// Zero the guest range `[lo, hi)` via the loader write path (best-effort: a range
    /// that escapes a mapped region is left untouched rather than aborting the host).
    /// No-op when `lo >= hi`. Used by the `madvise(MADV_DONTNEED)` edge/fallback zeroing.
    fn zero_range(&mut self, vm: &Vm, lo: u64, hi: u64) {
        if lo >= hi {
            return;
        }
        let len = (hi - lo) as usize;
        if self.try_resize_scratch(len) {
            let _ = vm.write_bytes(lo, &self.scratch);
        }
    }

    /// Allocate `aligned` bytes (page-aligned length) from the anonymous `mmap` arena,
    /// reclaiming freed space before growing (task-124). Returns the guest address, or
    /// `None` (→ `-ENOMEM`) if the arena isn't set up or is full. A zero-length request
    /// yields `None` (tracking/reusing a zero-length span is meaningless; real Linux
    /// rejects `mmap(len=0)` outright).
    ///
    /// Reuse order: first-fit the free list (a joined thread's stack lands there when it
    /// wasn't the most-recent mmap), then bump `mmap_base`. A reused span is re-zeroed so
    /// it reads back like a fresh, zero-filled anonymous map (the arena may hold stale
    /// bytes from the prior mapping). The bump path needs no zeroing: guest RAM above the
    /// high-water mark was never written.
    fn arena_alloc(&mut self, aligned: u64, vm: &Vm) -> Option<u64> {
        if self.mmap_base == 0 || aligned == 0 {
            return None;
        }
        // First-fit the free list. Take the whole span if it matches, else carve the
        // front and leave the remainder free.
        let fit = self
            .mmap_free
            .iter()
            .find(|&(_, &flen)| flen >= aligned)
            .map(|(&faddr, &flen)| (faddr, flen));
        if let Some((faddr, flen)) = fit {
            self.mmap_free.remove(&faddr);
            if flen > aligned {
                self.mmap_free.insert(faddr + aligned, flen - aligned);
            }
            // Re-zero so a reused mmap reads back zero like a fresh anonymous map.
            self.zero_span(faddr, aligned, vm);
            self.mmap_live.insert(faddr, aligned);
            return Some(faddr);
        }
        // Nothing reusable: grow the bump.
        if self.mmap_base + aligned <= self.mmap_limit {
            let a = self.mmap_base;
            self.mmap_base += aligned;
            self.mmap_live.insert(a, aligned);
            // A bump that starts below the high-water mark reuses space a top-of-bump
            // `munmap` rolled back over — those bytes may be stale, so re-zero the span
            // (the part above the mark, if any, is already zero → a harmless rewrite).
            // A bump entirely past the mark is never-written (already-zero) memory.
            if a < self.mmap_high {
                self.zero_span(a, aligned, vm);
            }
            self.mmap_high = self.mmap_high.max(self.mmap_base);
            Some(a)
        } else {
            None
        }
    }

    /// Return a `munmap`'d anonymous span to the arena (task-124). The common pthread
    /// case unmaps a whole tracked span: if it's the top of the bump, roll `mmap_base`
    /// back (instant, full reclaim, and it can cascade into an adjacent free span just
    /// below the new top); otherwise add it to the coalesced free list. A partial
    /// unmap (a prefix/suffix/hole of a tracked span) is split so accounting stays
    /// exact; an address we never handed out (or a `MAP_FIXED`/file-backed region,
    /// which we don't track) is ignored — like Linux `munmap` of an unmapped range,
    /// it just succeeds with no reclaim.
    fn arena_free(&mut self, addr: u64, len: u64) {
        if len == 0 {
            return;
        }
        let end = addr.saturating_add((len + 0xfff) & !0xfff);
        // Free every tracked live span that overlaps [addr, end), splitting on partial
        // overlap so a prefix/suffix left mapped stays tracked.
        let overlapping: Vec<(u64, u64)> = self
            .mmap_live
            .range(..end)
            .filter(|&(&saddr, &slen)| saddr + slen > addr)
            .map(|(&saddr, &slen)| (saddr, slen))
            .collect();
        for (saddr, slen) in overlapping {
            let send = saddr + slen;
            self.mmap_live.remove(&saddr);
            // Keep the un-unmapped prefix / suffix live (partial munmap).
            if saddr < addr {
                self.mmap_live.insert(saddr, addr - saddr);
            }
            if send > end {
                self.mmap_live.insert(end, send - end);
            }
            let fstart = saddr.max(addr);
            let fend = send.min(end);
            if fend > fstart {
                self.release_span(fstart, fend - fstart);
            }
        }
    }

    /// Return `[addr, addr+len)` to the arena: roll the bump back if it's the top (then
    /// keep rolling over any free span now at the top), else insert into the free list
    /// and coalesce with adjacent free spans (task-124).
    fn release_span(&mut self, addr: u64, len: u64) {
        if addr + len == self.mmap_base {
            self.mmap_base = addr;
            // A free span that now abuts the top folds back into the bump too, so
            // repeated top-of-bump frees fully unwind the high-water mark.
            while let Some((&faddr, &flen)) = self.mmap_free.range(..self.mmap_base).next_back() {
                if faddr + flen == self.mmap_base {
                    self.mmap_base = faddr;
                    self.mmap_free.remove(&faddr);
                } else {
                    break;
                }
            }
            return;
        }
        // Coalesce with a free span ending exactly at `addr` (the one just below).
        let mut start = addr;
        let mut end = addr + len;
        if let Some((&paddr, &plen)) = self.mmap_free.range(..start).next_back() {
            if paddr + plen == start {
                self.mmap_free.remove(&paddr);
                start = paddr;
            }
        }
        // Coalesce with a free span starting exactly at `end` (the one just above).
        if let Some(&nlen) = self.mmap_free.get(&end) {
            self.mmap_free.remove(&end);
            end += nlen;
        }
        self.mmap_free.insert(start, end - start);
    }

    /// Zero `[addr, addr+len)` in guest memory so a reused arena span reads back like a
    /// fresh, zero-filled anonymous map (task-124). Best-effort — a bogus length just
    /// skips the rezero rather than aborting the host, matching the anonymous MAP_FIXED
    /// rezero path.
    fn zero_span(&mut self, addr: u64, len: u64, vm: &Vm) {
        if self.try_resize_scratch(len as usize) {
            let _ = vm.write_bytes(addr, &self.scratch);
        }
    }

    /// Current monotonic nanoseconds since process start. Single-threaded: a
    /// deterministic virtual tick (each read advances a fixed quantum, #13).
    /// Threaded (after the first `clone`): the shared rate-controlled virtual clock
    /// (VCLK, decision-6) — each read ticks a smaller quantum and the driver credits
    /// real waits on expiry, so perceived time tracks guest progress yet stays
    /// decoupled from host wall-time (backend/load-invariant), never jumping backward
    /// across the switch (seeded from `clock_ns` at the flip).
    fn now_ns(&mut self) -> u64 {
        if self.threaded {
            self.mt_clock.tick(MT_CLOCK_TICK_NS)
        } else {
            self.clock_ns = self.clock_ns.wrapping_add(CLOCK_TICK_NS);
            self.clock_ns
        }
    }

    /// The current clock as `(seconds, nanoseconds)` since the reported epoch — a thin
    /// `(base + [`now_ns`](Self::now_ns))` split for the `clock_gettime`/`gettimeofday`
    /// family.
    fn tick_clock(&mut self) -> (i64, i64) {
        let ns = self.now_ns();
        let sec = CLOCK_BASE_SEC + (ns / 1_000_000_000) as i64;
        let nsec = (ns % 1_000_000_000) as i64;
        (sec, nsec)
    }

    /// Permit read-only host passthrough for exactly the given path (testing.md
    /// §12). Any `open` of a path not permitted returns `-ENOENT`.
    pub fn allow_read(&mut self, path: impl Into<PathBuf>) {
        self.fs.allow.push(path.into());
    }

    /// Serve `host` for any guest `open` of a path ending in `suffix` (except
    /// `glibc-hwcaps` probe variants). Lets a dynamic loader find a shared library
    /// (`libc.so.6`) from a checked-in fixture regardless of the absolute path
    /// baked into the binary.
    /// Permit read-only passthrough for every path under `dir` (an absolute host
    /// directory). Intended for an interpreter's stdlib tree.
    pub fn allow_dir(&mut self, dir: impl Into<PathBuf>) {
        self.fs.dirs.push(dir.into());
    }

    /// Serve an OCI image rootfs (chroot-like): every guest path resolves *inside*
    /// `root`, read and write, with escapes rejected. This is the OCI runner's
    /// filesystem; it takes precedence over the allowlist mechanisms above.
    pub fn serve_rootfs(&mut self, root: impl Into<PathBuf>) {
        self.fs.root = Some(root.into());
    }

    /// Permit **writable** passthrough for every path under `dir` (an absolute host
    /// directory) — real reads and writes, `O_CREAT`/`O_TRUNC` honored. Scope it to
    /// a per-test temp dir so a file-DB program (sqlite) can create and mutate its
    /// database and journal there, and nowhere else.
    pub fn allow_write_dir(&mut self, dir: impl Into<PathBuf>) {
        self.fs.write_dirs.push(dir.into());
    }

    pub fn serve_lib(&mut self, suffix: impl Into<Vec<u8>>, host: impl Into<PathBuf>) {
        self.fs.serve.push((suffix.into(), host.into()));
    }

    /// Fork this shim's OS state for a child process (OCI-4). The child inherits the
    /// fd table — File and pipe ends are shared (an `Rc` clone, POSIX open-file
    /// inheritance; pipe end counts bump); standard streams route the same way. Its
    /// stdout/stderr capture is fresh (the scheduler concatenates children's output
    /// in completion order), and the brk/mmap cursors + stdin + filesystem config are
    /// copied. No pending request carries into the child.
    /// Reseed the per-thread tid counter (task-126). `fork()` seeds `next_tid` from the
    /// PARENT's pid; the scheduler calls this with the CHILD's `pid + 1` after assigning the
    /// child its real pid, so a child that later escalates on `clone(CLONE_VM)` numbers its
    /// threads above its own main-thread tid (== its pid) instead of colliding with it.
    pub(crate) fn reseed_next_tid(&mut self, next: u64) {
        self.next_tid = next;
    }

    pub fn fork(&self) -> LinuxShim {
        let mut fd_table = BTreeMap::new();
        for (&fd, entry) in &self.fs.fd_table {
            let dup = match entry {
                Fd::Stdin => Fd::Stdin,
                Fd::Stdout => Fd::Stdout,
                Fd::Stderr => Fd::Stderr,
                Fd::File(rc) => Fd::File(rc.clone()),
                Fd::PipeRead(rc) => {
                    rc.lock().unwrap().readers += 1;
                    Fd::PipeRead(rc.clone())
                }
                Fd::PipeWrite(rc) => {
                    rc.lock().unwrap().writers += 1;
                    Fd::PipeWrite(rc.clone())
                }
                // POSIX: fork shares open sockets (same host fd) with the child.
                Fd::Socket(rc) => Fd::Socket(rc.clone()),
                Fd::Epoll(rc) => Fd::Epoll(rc.clone()),
                Fd::Event(rc) => Fd::Event(rc.clone()),
            };
            fd_table.insert(fd, dup);
        }
        LinuxShim {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: None,
            scripted: ScriptedSyscalls {
                responses: self.scripted.responses.clone(),
            },
            brk: self.brk,
            brk_limit: self.brk_limit,
            mmap_base: self.mmap_base,
            mmap_limit: self.mmap_limit,
            // A fork copies the parent's address space, so the child inherits the same
            // arena accounting: bump/high-water cursors and live/free spans (task-124).
            mmap_high: self.mmap_high,
            mmap_live: self.mmap_live.clone(),
            mmap_free: self.mmap_free.clone(),
            stdin: self.stdin.clone(),
            stdin_pos: self.stdin_pos,
            exe_path: self.exe_path.clone(),
            fs: FsPassthrough {
                allow: self.fs.allow.clone(),
                serve: self.fs.serve.clone(),
                dirs: self.fs.dirs.clone(),
                write_dirs: self.fs.write_dirs.clone(),
                root: self.fs.root.clone(),
                fd_table,
            },
            gap_syscalls: std::collections::HashSet::new(),
            pending_exec: None,
            pending_fork: false,
            pending_wait: None,
            pending_read: None,
            scratch: Vec::new(),
            // The child's pid is assigned by the scheduler after this fork; its parent
            // is us. (execve keeps the pid; only fork creates a new one.)
            pid: 0,
            ppid: self.pid,
            // Inherit the virtual clock so the child's time never predates the fork.
            clock_ns: self.clock_ns,
            // A freshly forked process starts single-threaded with its own tid range.
            next_tid: self.pid + 1,
            threaded: false,
            // A fork is a new process → its own virtual clock (seeded at its own flip).
            mt_clock: Arc::new(MtClock::default()),
            // fork inherits the parent's signal dispositions and masks (POSIX).
            sigactions: self.sigactions.clone(),
            altstack: self.altstack,
            sigmask: self.sigmask,
            // The child inherits the entropy mode; its PRNG continues from the parent's
            // state so a forked process's stream stays deterministic and non-colliding.
            entropy: self.entropy,
            rng_state: self.rng_state,
        }
    }

    /// Complete a `read` the scheduler parked (see [`Self::pending_read`]) after
    /// running pending writer children. Drains whatever is now in the pipe; an empty
    /// buffer reads as EOF (0), so a spurious wake can't loop forever.
    pub fn resume_read(&mut self, vm: &Vm, fd: u64, buf: u64, len: usize) -> u64 {
        self.do_read(vm, fd, buf, len)
    }

    /// Close every fd this process holds — called when the process exits so a pipe's
    /// writer/reader counts fall to zero and the other end sees EOF (POSIX: exit
    /// closes all descriptors).
    pub fn close_all_fds(&mut self) {
        for (_, entry) in std::mem::take(&mut self.fs.fd_table) {
            match entry {
                Fd::PipeRead(rc) => {
                    let mut b = rc.lock().unwrap();
                    b.readers = b.readers.saturating_sub(1);
                }
                Fd::PipeWrite(rc) => {
                    let mut b = rc.lock().unwrap();
                    b.writers = b.writers.saturating_sub(1);
                }
                _ => {}
            }
        }
    }

    /// Clone the `Fd` at `old` for a dup/`F_DUPFD` alias: an `Rc` clone shares the
    /// underlying file (and its seek offset) or pipe buffer; a pipe end's open count
    /// is bumped. `None` if `old` isn't open (→ `-EBADF`).
    fn clone_fd(&self, old: u64) -> Option<Fd> {
        match self.fs.fd_table.get(&old) {
            Some(Fd::Stdin) => Some(Fd::Stdin),
            Some(Fd::Stdout) => Some(Fd::Stdout),
            Some(Fd::Stderr) => Some(Fd::Stderr),
            Some(Fd::File(rc)) => Some(Fd::File(rc.clone())),
            Some(Fd::PipeRead(rc)) => {
                rc.lock().unwrap().readers += 1;
                Some(Fd::PipeRead(rc.clone()))
            }
            Some(Fd::PipeWrite(rc)) => {
                rc.lock().unwrap().writers += 1;
                Some(Fd::PipeWrite(rc.clone()))
            }
            Some(Fd::Socket(rc)) => Some(Fd::Socket(rc.clone())),
            Some(Fd::Epoll(rc)) => Some(Fd::Epoll(rc.clone())),
            Some(Fd::Event(rc)) => Some(Fd::Event(rc.clone())),
            None => None,
        }
    }

    /// Drop `fd` from the table, decrementing a pipe end's open count so a reader
    /// can see EOF once the last writer closes. Returns whether the fd existed.
    fn release(&mut self, fd: u64) -> bool {
        match self.fs.fd_table.remove(&fd) {
            Some(Fd::PipeRead(rc)) => {
                let mut b = rc.lock().unwrap();
                b.readers = b.readers.saturating_sub(1);
                true
            }
            Some(Fd::PipeWrite(rc)) => {
                let mut b = rc.lock().unwrap();
                b.writers = b.writers.saturating_sub(1);
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    /// Handle one `Exit::Syscall`. Returns `true` when the program has exited.
    ///
    /// An `Exit::Syscall` covers both long-mode `syscall` and i386 `int 0x80`; the
    /// CPU mode selects the ABI (§17.7). A `Compat32` VM uses the i386 numbering and
    /// 32-bit register/struct layout, dispatched separately so the x86-64 path below
    /// stays exactly as it was.
    pub fn handle(&mut self, cpu: &mut Vcpu, vm: &Vm) -> bool {
        if vm.cpu_mode() == CpuMode::Compat32 {
            return self.handle_i386(cpu, vm);
        }
        let nr = cpu.reg(Reg::Rax);
        match nr {
            SYS_WRITE => {
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let ret = self.do_write(vm, fd, buf, len);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_OPEN => {
                let path = cpu.reg(Reg::Rdi);
                let flags = cpu.reg(Reg::Rsi);
                let ret = self.do_open(vm, path, flags);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_OPENAT => {
                // openat(dirfd, path, flags, ...) — path/flags shift by one arg.
                let path = cpu.reg(Reg::Rsi);
                let flags = cpu.reg(Reg::Rdx);
                let ret = self.do_open(vm, path, flags);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_READ => {
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                if self.fs.pipe_would_block(fd) {
                    // Yield: the scheduler runs any pending writer child, then calls
                    // resume_read to complete this read (or EOF if nothing wrote).
                    self.pending_read = Some(PendingRead { fd, buf, len });
                    return true;
                }
                let ret = self.do_read(vm, fd, buf, len);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_CLOSE => {
                let fd = cpu.reg(Reg::Rdi);
                let ret = if self.release(fd) { 0 } else { EBADF };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_BRK => {
                // brk(0) queries the break; brk(addr) grows it within the limit.
                let req = cpu.reg(Reg::Rdi);
                if req != 0 && req >= self.brk && req <= self.brk_limit {
                    self.brk = req;
                }
                cpu.set_reg(Reg::Rax, self.brk);
                false
            }
            SYS_ARCH_PRCTL => {
                if cpu.reg(Reg::Rdi) == ARCH_SET_FS {
                    cpu.set_reg(Reg::FsBase, cpu.reg(Reg::Rsi)); // TLS base
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_SET_TID_ADDRESS => {
                cpu.set_reg(Reg::Rax, 1); // pretend tid 1
                false
            }
            SYS_READV => {
                // readv(fd, iov, iovcnt): scatter a read across the iovec buffers.
                // Stops early once a segment reads short (EOF), like the kernel.
                let fd = cpu.reg(Reg::Rdi);
                let iov = cpu.reg(Reg::Rsi);
                let cnt = cpu.reg(Reg::Rdx);
                let mut total = 0u64;
                for (base, len) in read_iovecs(vm, iov, cnt) {
                    if len == 0 {
                        continue;
                    }
                    let n = self.do_read(vm, fd, base, len);
                    if (n as i64) < 0 {
                        if total == 0 {
                            total = n;
                        }
                        break;
                    }
                    total += n;
                    if (n as usize) < len {
                        break; // short read → EOF
                    }
                }
                cpu.set_reg(Reg::Rax, total);
                false
            }
            SYS_WRITEV => {
                // writev(fd, iov, iovcnt): gather the iovec array and write it.
                let fd = cpu.reg(Reg::Rdi);
                let iov = cpu.reg(Reg::Rsi);
                let cnt = cpu.reg(Reg::Rdx);
                let mut total = 0u64;
                for (base, len) in read_iovecs(vm, iov, cnt) {
                    if len == 0 {
                        continue; // kernel ignores empty segments (base may be null)
                    }
                    // Guest-controlled base/len: a bad pointer must surface -EFAULT (or a
                    // short count if earlier segments already wrote), not panic the host.
                    if !self.try_fill_scratch(vm, base, len) {
                        if total == 0 {
                            total = EFAULT;
                        }
                        break;
                    }
                    match self.fs.fd_table.get(&fd) {
                        Some(Fd::Stdout) => self.stdout.extend_from_slice(&self.scratch),
                        Some(Fd::Stderr) => self.stderr.extend_from_slice(&self.scratch),
                        // A passthrough file: append at the current position. A write
                        // failure must not report success (was: error swallowed → the
                        // gather counted `len` regardless), matching the SYS_WRITE arm.
                        Some(Fd::File(rc)) => {
                            let failed = match rc.lock().unwrap().as_file_mut() {
                                Some(f) => f.write_all(&self.scratch).is_err(),
                                None => false, // read-only passthrough: swallow (like SYS_WRITE)
                            };
                            if failed {
                                if total == 0 {
                                    total = EBADF;
                                }
                                break;
                            }
                        }
                        Some(Fd::PipeWrite(rc)) => {
                            rc.lock().unwrap().data.extend(self.scratch.iter().copied())
                        }
                        Some(Fd::Socket(rc)) => {
                            // Honor the host write result like SYS_WRITE: a short write or
                            // error (EPIPE, backpressure) must not report full success.
                            let h = rc.as_raw_fd();
                            let n = unsafe {
                                libc::write(h, self.scratch.as_ptr() as *const libc::c_void, len)
                            };
                            if n < 0 {
                                if total == 0 {
                                    total = host_errno();
                                }
                                break;
                            }
                            total += n as u64;
                            if (n as usize) < len {
                                break; // short write → stop the gather
                            }
                            continue; // already counted the socket bytes
                        }
                        // An eventfd write is 8 bytes; honor the host result like a socket.
                        Some(Fd::Event(rc)) => {
                            let h = rc.as_raw_fd();
                            let n = unsafe {
                                libc::write(h, self.scratch.as_ptr() as *const libc::c_void, len)
                            };
                            if n < 0 {
                                if total == 0 {
                                    total = host_errno();
                                }
                                break;
                            }
                            total += n as u64;
                            if (n as usize) < len {
                                break;
                            }
                            continue;
                        }
                        // Writing to a read end / stdin / epoll fd / absent fd is not a
                        // successful write — return -EBADF (was: fell through to
                        // `total += len`, reporting bytes it never wrote).
                        Some(Fd::PipeRead(_)) | Some(Fd::Stdin) | Some(Fd::Epoll(_)) | None => {
                            if total == 0 {
                                total = EBADF;
                            }
                            break;
                        }
                    }
                    total += len as u64;
                }
                cpu.set_reg(Reg::Rax, total);
                false
            }
            SYS_MMAP => {
                // mmap(addr, len, prot, flags, fd, offset). MAP_FIXED honors the
                // address as-is (the flat region is already RW). Anonymous maps come
                // from the bump arena. File-backed maps (fd != -1, as glibc's ld.so
                // uses to map libc.so.6) read the file's bytes at `offset` into the
                // chosen guest address.
                const MAP_FIXED: u64 = 0x10;
                let addr = cpu.reg(Reg::Rdi);
                let len = cpu.reg(Reg::Rsi);
                let flags = cpu.reg(Reg::R10);
                // fd is an `int` in the kernel ABI: callers pass -1 as a 32-bit
                // value (glibc leaves R8's upper half zero), so truncate before
                // testing for "anonymous".
                let fd = cpu.reg(Reg::R8) as u32 as i32;
                let off = cpu.reg(Reg::R9);
                let target = if flags & MAP_FIXED != 0 {
                    addr
                } else {
                    let aligned = (len + 0xfff) & !0xfff;
                    match self.arena_alloc(aligned, vm) {
                        Some(a) => a,
                        None => {
                            cpu.set_reg(Reg::Rax, ENOMEM);
                            return false;
                        }
                    }
                };
                if fd >= 0 {
                    // File-backed: copy the file's bytes in (the tail past EOF stays
                    // zero, since guest RAM is zero-initialized).
                    if let Some(rc) = self.fs.file(fd as u64) {
                        let entry = rc.lock().unwrap();
                        if let Some(file) = entry.as_file() {
                            // `try_resize_scratch` so a bogus length can't abort the host;
                            // `write_bytes` is best-effort (the target was just allocated,
                            // but a guest MAP_FIXED into an unmapped span mustn't panic).
                            if self.try_resize_scratch(len as usize) {
                                if let Ok(n) = file.read_at(&mut self.scratch, off) {
                                    let _ = vm.write_bytes(target, &self.scratch[..n]);
                                }
                            }
                        }
                    }
                } else if flags & MAP_FIXED != 0 {
                    // Anonymous MAP_FIXED (a segment's bss) must present zeroed pages,
                    // overwriting whatever a prior file mapping left there. Guest RAM is
                    // already zero-initialized, so a too-large length just skips the
                    // explicit rezero rather than aborting the host.
                    if self.try_resize_scratch(len as usize) {
                        let _ = vm.write_bytes(target, &self.scratch);
                    }
                }
                cpu.set_reg(Reg::Rax, target);
                false
            }
            SYS_MUNMAP => {
                // Return the freed anonymous span to the arena so a thread-churning guest
                // (each pthread mmaps a stack, then munmaps it on join) can reuse it
                // instead of exhausting the bump (task-124). Always succeeds (0): an
                // address the guest never got from us is simply not in our accounting.
                let addr = cpu.reg(Reg::Rdi);
                let len = cpu.reg(Reg::Rsi);
                self.arena_free(addr, len);
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_MPROTECT => {
                // No-op: page protections aren't enforced in the flat model (§4.2).
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_STAT | SYS_LSTAT => {
                let path = read_cstr(vm, cpu.reg(Reg::Rdi));
                let meta = self
                    .fs
                    .resolve_host(&path)
                    .or_else(|| self.fs.resolve_host_write(&path))
                    .and_then(|p| std::fs::metadata(p).ok());
                let ret = match meta {
                    Some(m) => {
                        write_stat(vm, cpu.reg(Reg::Rsi), &m);
                        0
                    }
                    None => ENOENT,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_FSTAT => {
                let fd = cpu.reg(Reg::Rdi);
                let meta = self
                    .fs
                    .file(fd)
                    .and_then(|rc| rc.lock().unwrap().metadata());
                let ret = match meta {
                    Some(m) => {
                        write_stat(vm, cpu.reg(Reg::Rsi), &m);
                        0
                    }
                    // stdin/stdout/stderr: present them as character devices so an
                    // interpreter's stream setup (fstat 0/1/2) succeeds.
                    None if fd < 3 => {
                        write_chr_stat(vm, cpu.reg(Reg::Rsi));
                        0
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_PREAD64 => {
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let off = cpu.reg(Reg::R10);
                let ret = match self.fs.file(fd) {
                    Some(rc) => match rc.lock().unwrap().as_file() {
                        Some(file) => {
                            if !self.try_resize_scratch(len) {
                                ENOMEM // bogus length → -ENOMEM, no host abort
                            } else {
                                match file.read_at(&mut self.scratch, off) {
                                    Ok(n) => match vm.write_bytes(buf, &self.scratch[..n]) {
                                        Ok(()) => n as u64,
                                        Err(_) => EFAULT, // unmapped dest → -EFAULT, no panic
                                    },
                                    Err(_) => EBADF,
                                }
                            }
                        }
                        None => EBADF,
                    },
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_NEWFSTATAT => {
                // fstatat(dirfd, path, statbuf, flags). Empty path + AT_EMPTY_PATH
                // (fstat) → the dirfd's file; otherwise resolve the (absolute) path.
                let path = read_cstr(vm, cpu.reg(Reg::Rsi));
                let meta = if path.is_empty() {
                    self.fs
                        .file(cpu.reg(Reg::Rdi))
                        .and_then(|rc| rc.lock().unwrap().metadata())
                } else {
                    self.fs
                        .resolve_host(&path)
                        .and_then(|p| std::fs::metadata(p).ok())
                };
                let ret = match meta {
                    Some(m) => {
                        write_stat(vm, cpu.reg(Reg::Rdx), &m);
                        0
                    }
                    None => ENOENT,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SET_ROBUST_LIST => {
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_RSEQ => {
                cpu.set_reg(Reg::Rax, (-38i64) as u64); // -ENOSYS: glibc disables rseq
                false
            }
            SYS_PRLIMIT64 => {
                // prlimit64(pid, resource, new_limit, old_limit). Branch on `resource`
                // (the old arm ignored it and answered an 8 MiB limit for everything — so
                // a NOFILE query got an "8 MiB fd limit"). RLIMIT_STACK must match the
                // #14 stack budget; new_limit is accepted and ignored.
                const RLIMIT_STACK: u64 = 3;
                const RLIMIT_NOFILE: u64 = 7;
                let resource = cpu.reg(Reg::Rsi);
                let old = cpu.reg(Reg::R10);
                let (soft, hard) = match resource {
                    RLIMIT_STACK => (8u64 * 1024 * 1024, u64::MAX),
                    RLIMIT_NOFILE => (1024, 4096),
                    _ => (u64::MAX, u64::MAX),
                };
                if old != 0 {
                    let mut buf = [0u8; 16];
                    buf[0..8].copy_from_slice(&soft.to_le_bytes());
                    buf[8..16].copy_from_slice(&hard.to_le_bytes());
                    let _ = vm.write_bytes(old, &buf);
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_GETRANDOM => {
                // Entropy per the selected mode (task-128): a reproducible PRNG stream
                // (Deterministic, default) or real host randomness (HostEntropy, for TLS).
                let buf = cpu.reg(Reg::Rdi);
                let len = cpu.reg(Reg::Rsi) as usize;
                if self.fill_scratch_entropy(len) {
                    let _ = vm.write_bytes(buf, &self.scratch);
                }
                cpu.set_reg(Reg::Rax, len as u64);
                false
            }
            SYS_IOCTL => {
                // FIONBIO/FIONREAD on a host socket forward to the real fd — openssl uses
                // ioctl(FIONBIO) to put a socket in non-blocking mode (task-215). Everything
                // else (ttys) reports -ENOTTY: no ttys in the harness → isatty() is false.
                let fd = cpu.reg(Reg::Rdi);
                let req = cpu.reg(Reg::Rsi);
                let argp = cpu.reg(Reg::Rdx);
                const FIONBIO: u64 = 0x5421;
                const FIONREAD: u64 = 0x541B;
                if let (Some(h), true) = (self.fs.socket_fd(fd), req == FIONBIO || req == FIONREAD)
                {
                    let ret = if req == FIONBIO {
                        let mut b = [0u8; 4];
                        if vm.read_bytes(argp, &mut b).is_err() {
                            EFAULT
                        } else {
                            let mut on = i32::from_le_bytes(b);
                            let r = unsafe { libc::ioctl(h, libc::FIONBIO, &mut on) };
                            if r < 0 {
                                host_errno()
                            } else {
                                0
                            }
                        }
                    } else {
                        let mut n: libc::c_int = 0;
                        let r = unsafe { libc::ioctl(h, libc::FIONREAD, &mut n) };
                        if r < 0 {
                            host_errno()
                        } else {
                            if argp != 0 {
                                let _ = vm.write_bytes(argp, &(n as i32).to_le_bytes());
                            }
                            0
                        }
                    };
                    cpu.set_reg(Reg::Rax, ret);
                    return false;
                }
                cpu.set_reg(Reg::Rax, ENOTTY);
                false
            }
            SYS_RT_SIGACTION => {
                // Record the disposition (process-wide) and read the old one back; no
                // delivery (P3). Go's `initsig` queries every signal to build `fwdSig`,
                // so a stub that skips the old-write feeds it stack garbage.
                let sig = cpu.reg(Reg::Rdi) as usize;
                let new = cpu.reg(Reg::Rsi);
                let old = cpu.reg(Reg::Rdx);
                if sig < 1 || sig > self.sigactions.len() {
                    cpu.set_reg(Reg::Rax, EINVAL);
                    return false;
                }
                let slot = &mut self.sigactions[sig - 1];
                if old != 0 {
                    let _ = vm.write_bytes(old, slot);
                }
                if new != 0 {
                    let mut buf = [0u8; 32];
                    if vm.read_bytes(new, &mut buf).is_ok() {
                        *slot = buf;
                    }
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_RT_SIGPROCMASK => {
                // Single-threaded path: the mask lives on the shim. (A threaded process
                // routes this through `handle_mt` to per-thread `ThreadCtx` state.)
                let ret = do_sigprocmask(cpu, vm, &mut self.sigmask);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SIGALTSTACK => {
                // Single-threaded path (see `handle_mt` for the per-thread one). Record
                // and read back the alt stack; no delivery (P3), but a `sigaltstack(nil,
                // &old)` query must read `SS_DISABLE`, not uninitialized guest memory.
                let ret = do_sigaltstack(cpu, vm, &mut self.altstack);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_MADVISE => {
                // madvise(addr=RDI, length=RSI, advice=RDX). Mostly advisory, but
                // MADV_DONTNEED is load-bearing: on anonymous memory Linux guarantees
                // that pages read back as **zero** after it. Go's scavenger relies on
                // this — it returns scavenged spans to the heap with `needzero == 0`
                // and `mallocgc` then skips zeroing, trusting the kernel already did.
                // Our `reserve()` span never re-zeroes on its own, so a plain no-op
                // leaves the old bytes in place; `mallocgc` hands the slot out dirty and
                // a `&T{...}` composite literal (which only writes its named fields,
                // trusting the rest is zero) reads stale pointers — the task-161 heap
                // corruption. So we zero the range to match the kernel — and, on a
                // host-mapped (`MAP_NORESERVE`) region, `madvise(MADV_DONTNEED)` the host
                // backing pages too so physical RSS is actually returned to the OS (a
                // long-running Go server's scavenger otherwise never shrinks host memory,
                // task-131). The host madvise refaults the pages as zero, satisfying the
                // zeroing guarantee for the pages it covers. MADV_FREE (lazy) has no
                // zeroing guarantee and Go re-zeroes those spans itself, so it and every
                // other advice stay a no-op success.
                const MADV_DONTNEED: u64 = 4;
                if cpu.reg(Reg::Rdx) == MADV_DONTNEED {
                    let addr = cpu.reg(Reg::Rdi);
                    let len = cpu.reg(Reg::Rsi);
                    self.madvise_dontneed(vm, addr, len);
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_FUTEX => {
                // futex(uaddr, op, val, ...). Single-threaded harness: a WAKE is a
                // no-op, and a WAIT can only be a lost race — if the word already
                // differs from `val` return -EAGAIN like the kernel. If it still
                // matches, no other thread exists to change it, so a real block would
                // deadlock; we return -EAGAIN rather than block or panic. Guest input
                // must never crash the host (harden #1); a guest that genuinely needs
                // a blocking futex needs the mt substrate (plan D4), not this shim.
                const FUTEX_CMD_MASK: u64 = 0x7f; // strip PRIVATE/CLOCK flags
                const FUTEX_WAIT: u64 = 0;
                const EAGAIN: u64 = (-11i64) as u64;
                const EFAULT: u64 = (-14i64) as u64;
                let op = cpu.reg(Reg::Rsi) & FUTEX_CMD_MASK;
                let ret = if op == FUTEX_WAIT {
                    let uaddr = cpu.reg(Reg::Rdi);
                    let mut w = [0u8; 4];
                    // Unmapped futex word: report -EFAULT like the kernel instead of
                    // unwrapping (a guest-controlled pointer must not panic the host).
                    match vm.read_bytes(uaddr, &mut w) {
                        Ok(()) => EAGAIN, // word matches → would-block; changed → lost race; both -EAGAIN
                        Err(_) => EFAULT,
                    }
                } else {
                    0 // WAKE and friends: nobody to wake
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_LSEEK => {
                // Seek a passthrough file; unknown fd → -EBADF.
                let fd = cpu.reg(Reg::Rdi);
                let off = cpu.reg(Reg::Rsi) as i64;
                let whence = cpu.reg(Reg::Rdx);
                let ret = self.fs.with_file(fd, |f| {
                    let pos = match whence {
                        0 => std::io::SeekFrom::Start(off as u64),
                        1 => std::io::SeekFrom::Current(off),
                        _ => std::io::SeekFrom::End(off),
                    };
                    match std::io::Seek::seek(f, pos) {
                        Ok(p) => p,
                        Err(_) => (-29i64) as u64, // -ESPIPE
                    }
                });
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_PWRITE64 => {
                // pwrite(fd, buf, len, off): positioned write, file offset untouched.
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let off = cpu.reg(Reg::R10);
                if !self.try_fill_scratch(vm, buf, len) {
                    cpu.set_reg(Reg::Rax, EFAULT); // unmapped/bogus source → -EFAULT, no panic
                    return false;
                }
                let ret = self
                    .fs
                    .with_file(fd, |f| match f.write_at(&self.scratch, off) {
                        Ok(n) => n as u64,
                        Err(_) => EBADF,
                    });
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_FTRUNCATE => {
                let fd = cpu.reg(Reg::Rdi);
                let size = cpu.reg(Reg::Rsi);
                let ret = self.fs.with_file(fd, |f| match f.set_len(size) {
                    Ok(()) => 0,
                    Err(_) => EBADF,
                });
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_PIPE | SYS_PIPE2 => {
                // pipe(fds) / pipe2(fds, flags): allocate one shared buffer, hand out
                // a read end and a write end, and write the two fd numbers to the
                // guest `int[2]` at RDI. pipe2's O_NONBLOCK (task-232) is honored on the
                // read end so a self-pipe / event-loop guest gets an immediate `-EAGAIN`
                // on an empty pipe; O_CLOEXEC is still ignored — cloexec matters only once
                // execve preserves fds (oci-multiprocess-plan.md §4), a later rung.
                let ptr = cpu.reg(Reg::Rdi);
                let nonblocking =
                    nr == SYS_PIPE2 && (cpu.reg(Reg::Rsi) & (libc::O_NONBLOCK as u64)) != 0;
                let pipe = Arc::new(Mutex::new(PipeBuf {
                    data: VecDeque::new(),
                    writers: 1,
                    readers: 1,
                    nonblocking,
                }));
                let rfd = self.fs.alloc_fd();
                self.fs.fd_table.insert(rfd, Fd::PipeRead(pipe.clone()));
                let wfd = self.fs.alloc_fd();
                self.fs.fd_table.insert(wfd, Fd::PipeWrite(pipe));
                let mut fds = [0u8; 8];
                fds[0..4].copy_from_slice(&(rfd as u32).to_le_bytes());
                fds[4..8].copy_from_slice(&(wfd as u32).to_le_bytes());
                let _ = vm.write_bytes(ptr, &fds);
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_DUP | SYS_DUP2 => {
                // dup(old)->lowest-free; dup2(old,new)->new. Alias the fd through the
                // table: an `Rc` clone shares the underlying file (and its seek
                // offset, POSIX); std streams clone to the same stream. `dup2` onto an
                // open fd overwrites it (an implicit close of the target).
                let old = cpu.reg(Reg::Rdi);
                let new = if nr == SYS_DUP2 {
                    cpu.reg(Reg::Rsi)
                } else {
                    self.fs.alloc_fd()
                };
                let dup = self.clone_fd(old);
                let ret = if old == new {
                    // dup2(fd, fd) is a no-op that returns fd — but only if fd is valid.
                    if dup.is_some() {
                        new
                    } else {
                        EBADF
                    }
                } else {
                    match dup {
                        Some(entry) => {
                            self.release(new); // dup2 implicitly closes an open target
                            self.fs.fd_table.insert(new, entry);
                            new
                        }
                        None => EBADF,
                    }
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_CHMOD | SYS_FCHMOD | SYS_CHOWN | SYS_FCHOWN => {
                // Permissions/ownership aren't modeled — sqlite fchmods a new DB to
                // match its directory; report success without touching the host file.
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_FSYNC | SYS_FDATASYNC => {
                // Durability isn't observable in-process; flush and report success.
                let fd = cpu.reg(Reg::Rdi);
                if let Some(rc) = self.fs.file(fd) {
                    if let Some(f) = rc.lock().unwrap().as_file_mut() {
                        let _ = f.flush();
                    }
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_UNLINK | SYS_UNLINKAT => {
                // sqlite deletes its `-journal`/`-wal` on a clean commit. `unlink`
                // takes the path in RDI; `unlinkat` in RSI (after dirfd).
                let path_reg = if nr == SYS_UNLINK { Reg::Rdi } else { Reg::Rsi };
                let path = read_cstr(vm, cpu.reg(path_reg));
                let ret = match self.fs.resolve_host_write(&path) {
                    Some(host) => match std::fs::remove_file(&host) {
                        Ok(()) => 0,
                        Err(_) => ENOENT,
                    },
                    None => EACCES,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_MKDIR | SYS_MKDIRAT => {
                // caddy creates its data dir (local CA / cert storage) on boot. `mkdir`
                // takes the path in RDI; `mkdirat` in RSI (after dirfd). Gated to a
                // writable passthrough dir; a pre-existing dir reports success (EEXIST is
                // benign for the callers we serve).
                let path_reg = if nr == SYS_MKDIR { Reg::Rdi } else { Reg::Rsi };
                let path = read_cstr(vm, cpu.reg(path_reg));
                let ret = match self.fs.resolve_host_write(&path) {
                    Some(host) => match std::fs::create_dir_all(&host) {
                        Ok(()) => 0,
                        Err(_) => EACCES,
                    },
                    None => EACCES,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SYSINFO => {
                // sysinfo(struct sysinfo*): synthetic but plausible memory/uptime so a
                // guest sizing caches (Go's runtime, glibc) gets sane non-zero values.
                // x86-64 layout: uptime@0, loads[3]@8, totalram@32, freeram@40,
                // sharedram@48, bufferram@56, totalswap@64, freeswap@72, procs@80(u16),
                // totalhigh@88, freehigh@96, mem_unit@104(u32).
                let buf = cpu.reg(Reg::Rdi);
                if buf != 0 {
                    let mut si = [0u8; 112];
                    let put = |b: &mut [u8], off: usize, v: u64| {
                        b[off..off + 8].copy_from_slice(&v.to_le_bytes());
                    };
                    put(&mut si, 0, 3600); // uptime (s)
                    put(&mut si, 32, 4 << 30); // totalram = 4 GiB
                    put(&mut si, 40, 2 << 30); // freeram  = 2 GiB
                    put(&mut si, 64, 1 << 30); // totalswap
                    put(&mut si, 72, 1 << 30); // freeswap
                    si[80..82].copy_from_slice(&1u16.to_le_bytes()); // procs
                    si[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit = 1 byte
                    let _ = vm.write_bytes(buf, &si);
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_RENAME | SYS_RENAMEAT | SYS_RENAMEAT2 => {
                // caddy writes its generated cert to a temp name then renames it into
                // place (atomic replace). `rename` takes old/new in RDI/RSI; the `*at`
                // forms put the paths after each dirfd (old in RSI, new in R10). Both
                // paths must resolve to a writable passthrough dir.
                //
                // renameat2 adds a flags word in R8: RENAME_NOREPLACE (fail with EEXIST
                // if the destination exists) and RENAME_EXCHANGE (atomically swap the two
                // entries). A plain `std::fs::rename` is an unconditional replace, so
                // silently taking that path for a flagged renameat2 would let NOREPLACE
                // clobber an existing file and turn EXCHANGE into a one-way move that
                // loses the destination. Honor the flags for real via the raw
                // `renameat2` syscall on the resolved host paths (AT_FDCWD + absolute
                // paths ⇒ the dirfd is irrelevant). rename/renameat carry no flags word,
                // so they stay a plain replace (caddy depends on that).
                let (old_reg, new_reg) = if nr == SYS_RENAME {
                    (Reg::Rdi, Reg::Rsi)
                } else {
                    (Reg::Rsi, Reg::R10)
                };
                let flags = if nr == SYS_RENAMEAT2 {
                    cpu.reg(Reg::R8) as libc::c_uint
                } else {
                    0
                };
                let old = read_cstr(vm, cpu.reg(old_reg));
                let new = read_cstr(vm, cpu.reg(new_reg));
                let ret = match (
                    self.fs.resolve_host_write(&old),
                    self.fs.resolve_host_write(&new),
                ) {
                    (Some(o), Some(n)) => {
                        match (
                            std::ffi::CString::new(o.as_os_str().as_bytes()),
                            std::ffi::CString::new(n.as_os_str().as_bytes()),
                        ) {
                            (Ok(oc), Ok(nc)) => {
                                // AT_FDCWD for both dirfds; the resolved paths are
                                // absolute so the cwd is never consulted. flags == 0
                                // reproduces the old plain-replace behavior exactly.
                                let r = unsafe {
                                    libc::syscall(
                                        libc::SYS_renameat2,
                                        libc::AT_FDCWD,
                                        oc.as_ptr(),
                                        libc::AT_FDCWD,
                                        nc.as_ptr(),
                                        flags,
                                    )
                                };
                                if r < 0 {
                                    host_errno()
                                } else {
                                    0
                                }
                            }
                            // A NUL byte in a resolved host path can't be passed to the
                            // kernel; treat it as a denied access like an unresolvable
                            // path rather than panicking.
                            _ => EACCES,
                        }
                    }
                    _ => EACCES,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_ACCESS => {
                // Exists iff it resolves to a passthrough host path (read or write).
                let path = read_cstr(vm, cpu.reg(Reg::Rdi));
                let ok = self
                    .fs
                    .resolve_host(&path)
                    .or_else(|| self.fs.resolve_host_write(&path))
                    .is_some_and(|p| p.exists());
                cpu.set_reg(Reg::Rax, if ok { 0 } else { ENOENT });
                false
            }
            SYS_FCNTL => {
                // F_DUPFD(_CLOEXEC): duplicate RDI to the lowest free fd ≥ RDX and
                // return it. A shell moves its script/redirect fds above 10 this way
                // (fcntl(fd, F_DUPFD, 10)); returning a blanket 0 told the guest the
                // dup landed on stdin and corrupted its redirection. The CLOEXEC
                // variant duplicates identically — the close-on-exec bit itself is the
                // separate deferred O_CLOEXEC work. Other commands stay benign
                // (F_SETFD/F_GETFL/F_SETLK… → 0).
                const F_DUPFD: u64 = 0;
                const F_GETFL: u64 = 3;
                const F_SETFL: u64 = 4;
                const F_DUPFD_CLOEXEC: u64 = 1030;
                let cmd = cpu.reg(Reg::Rsi);
                let ret = match cmd {
                    F_DUPFD | F_DUPFD_CLOEXEC => {
                        let old = cpu.reg(Reg::Rdi);
                        match self.clone_fd(old) {
                            Some(entry) => {
                                let new = self.fs.alloc_fd_from(cpu.reg(Reg::Rdx));
                                self.fs.fd_table.insert(new, entry);
                                new
                            }
                            None => EBADF,
                        }
                    }
                    // F_GETFL/F_SETFL on a host-backed fd (socket/eventfd/epoll) must be
                    // truthful about O_NONBLOCK: a guest that thinks it cleared O_NONBLOCK
                    // and then reads would otherwise block a host thread under the shim
                    // lock. Forward to the host fd; mask F_SETFL to O_NONBLOCK (the guest
                    // owns the fd's I/O mode, not host signal-driven-IO machinery). Other
                    // fds keep the benign-0 the shell corpus depends on (go-caddy P4).
                    F_GETFL if self.fs.host_io_fd(cpu.reg(Reg::Rdi)).is_some() => {
                        let h = self.fs.host_io_fd(cpu.reg(Reg::Rdi)).unwrap();
                        let r = unsafe { libc::fcntl(h, libc::F_GETFL) };
                        if r < 0 {
                            host_errno()
                        } else {
                            r as u64
                        }
                    }
                    F_SETFL if self.fs.host_io_fd(cpu.reg(Reg::Rdi)).is_some() => {
                        let h = self.fs.host_io_fd(cpu.reg(Reg::Rdi)).unwrap();
                        let flags = (cpu.reg(Reg::Rdx) as libc::c_int) & libc::O_NONBLOCK;
                        let r = unsafe { libc::fcntl(h, libc::F_SETFL, flags) };
                        if r < 0 {
                            host_errno()
                        } else {
                            0
                        }
                    }
                    // F_SETFL on a pipe read end tracks O_NONBLOCK on the shared buffer
                    // (task-232) so a subsequent empty read gets `-EAGAIN` inline instead of
                    // parking. F_GETFL reports it back truthfully.
                    F_SETFL if self.fs.pipe_read(cpu.reg(Reg::Rdi)).is_some() => {
                        let nb = (cpu.reg(Reg::Rdx) & (libc::O_NONBLOCK as u64)) != 0;
                        if let Some(rc) = self.fs.pipe_read(cpu.reg(Reg::Rdi)) {
                            rc.lock().unwrap().nonblocking = nb;
                        }
                        0
                    }
                    F_GETFL if self.fs.pipe_read(cpu.reg(Reg::Rdi)).is_some() => {
                        let nb = self
                            .fs
                            .pipe_read(cpu.reg(Reg::Rdi))
                            .map(|rc| rc.lock().unwrap().nonblocking)
                            .unwrap_or(false);
                        if nb {
                            libc::O_NONBLOCK as u64
                        } else {
                            0
                        }
                    }
                    _ => 0,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_GETPID | SYS_GETTID => {
                // Single-threaded guest model: tid == pid.
                cpu.set_reg(Reg::Rax, self.pid);
                false
            }
            SYS_GETPPID => {
                cpu.set_reg(Reg::Rax, self.ppid);
                false
            }
            SYS_TIME => {
                let (t, _) = self.tick_clock();
                let tloc = cpu.reg(Reg::Rdi);
                if tloc != 0 {
                    let _ = vm.write_bytes(tloc, &t.to_le_bytes());
                }
                cpu.set_reg(Reg::Rax, t as u64);
                false
            }
            SYS_GETCWD => {
                // Report "/" — deterministic; the programs we run don't depend on it.
                let buf = cpu.reg(Reg::Rdi);
                let _ = vm.write_bytes(buf, b"/\0");
                cpu.set_reg(Reg::Rax, 2); // length including the NUL
                false
            }
            SYS_READLINK => {
                // No symlinks in the harness (e.g. /proc/self/exe) → let the guest
                // fall back to argv[0]/PYTHONHOME.
                cpu.set_reg(Reg::Rax, ENOENT);
                false
            }
            SYS_READLINKAT => {
                // readlinkat(dirfd=RDI, pathname=RSI, buf=RDX, bufsiz=R10). Only
                // /proc/self/exe is meaningful here — Go's `os.Executable` reads it
                // (task-162). Resolve it to the recorded entrypoint path; anything else
                // (or an unset path) → -ENOENT, like `SYS_READLINK`.
                let path = read_cstr(vm, cpu.reg(Reg::Rsi));
                let ret = if path == b"/proc/self/exe" && !self.exe_path.is_empty() {
                    let buf = cpu.reg(Reg::Rdx);
                    let bufsiz = cpu.reg(Reg::R10) as usize;
                    // readlink truncates to bufsiz and does NOT NUL-terminate; the
                    // return value is the byte count written.
                    let n = self.exe_path.len().min(bufsiz);
                    match vm.write_bytes(buf, &self.exe_path[..n]) {
                        Ok(()) => n as u64,
                        Err(_) => EFAULT,
                    }
                } else {
                    ENOENT
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_UNAME => {
                // uname(buf): fill a plausible `struct utsname` (6 × char[65], all
                // NUL-padded) so the Go runtime/`os` see a sane Linux/x86_64 host
                // instead of the zeroed buffer an -ENOSYS default would leave (task-162).
                const FIELD: usize = 65;
                let fields: [&[u8]; 6] = [
                    b"Linux",         // sysname
                    b"x86jit",        // nodename
                    b"6.1.0",         // release (modern enough for the runtime's checks)
                    b"#1 SMP x86jit", // version
                    b"x86_64",        // machine
                    b"(none)",        // domainname
                ];
                let mut uts = [0u8; FIELD * 6];
                for (i, f) in fields.iter().enumerate() {
                    let off = i * FIELD;
                    let n = f.len().min(FIELD - 1); // leave room for the NUL
                    uts[off..off + n].copy_from_slice(&f[..n]);
                }
                let ret = match vm.write_bytes(cpu.reg(Reg::Rdi), &uts) {
                    Ok(()) => 0,
                    Err(_) => EFAULT,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_GETDENTS64 => {
                // Stream `struct linux_dirent64` records for an open directory into
                // the guest buffer until it's full; 0 when exhausted. An
                // interpreter's importer lists directories to discover modules.
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let count = cpu.reg(Reg::Rdx) as usize;
                let mut out = Vec::new();
                if let Some(rc) = self.fs.file(fd) {
                    if let OpenEntry::Dir(d) = &mut *rc.lock().unwrap() {
                        while d.pos < d.entries.len() {
                            let e = &d.entries[d.pos];
                            let reclen = (19usize + e.name.len() + 1).div_ceil(8) * 8; // header 19 + name + NUL
                            if out.len() + reclen > count {
                                break;
                            }
                            let mut rec = vec![0u8; reclen];
                            rec[0..8].copy_from_slice(&e.ino.to_le_bytes()); // d_ino
                            rec[8..16].copy_from_slice(&(d.pos as u64 + 1).to_le_bytes()); // d_off
                            rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes()); // d_reclen
                            rec[18] = e.dtype; // d_type
                            rec[19..19 + e.name.len()].copy_from_slice(&e.name); // d_name + NUL pad
                            out.extend_from_slice(&rec);
                            d.pos += 1;
                        }
                    }
                }
                let _ = vm.write_bytes(buf, &out);
                cpu.set_reg(Reg::Rax, out.len() as u64);
                false
            }
            SYS_CLOCK_GETTIME => {
                // Monotonic virtual clock → deterministic but advancing (#13).
                // timespec { i64 sec, i64 nsec } at RSI.
                let (sec, nsec) = self.tick_clock();
                let mut ts = [0u8; 16];
                ts[0..8].copy_from_slice(&sec.to_le_bytes());
                ts[8..16].copy_from_slice(&nsec.to_le_bytes());
                let _ = vm.write_bytes(cpu.reg(Reg::Rsi), &ts);
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_GETTIMEOFDAY => {
                // timeval { i64 sec, i64 usec } at RDI.
                let (sec, nsec) = self.tick_clock();
                let mut tv = [0u8; 16];
                tv[0..8].copy_from_slice(&sec.to_le_bytes());
                tv[8..16].copy_from_slice(&(nsec / 1000).to_le_bytes());
                let _ = vm.write_bytes(cpu.reg(Reg::Rdi), &tv);
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_NANOSLEEP | SYS_CLOCK_NANOSLEEP => {
                // Advance the virtual clock past the requested duration and return
                // success — no real wait, but time moves, so a sleep-until-deadline
                // loop terminates (#13). `timespec` is { i64 sec, i64 nsec }.
                //   nanosleep(req*, rem*):                    req at RDI (relative)
                //   clock_nanosleep(id, flags, req*, rem*):   req at RDX; RSI flags,
                //     TIMER_ABSTIME (bit 0) makes req an absolute deadline.
                const TIMER_ABSTIME: u64 = 1;
                let (req_ptr, abs) = if nr == SYS_CLOCK_NANOSLEEP {
                    (cpu.reg(Reg::Rdx), cpu.reg(Reg::Rsi) & TIMER_ABSTIME != 0)
                } else {
                    (cpu.reg(Reg::Rdi), false)
                };
                let mut ts = [0u8; 16];
                if vm.read_bytes(req_ptr, &mut ts).is_ok() {
                    let sec = i64::from_le_bytes(ts[0..8].try_into().unwrap());
                    let nsec = i64::from_le_bytes(ts[8..16].try_into().unwrap());
                    let want = (sec.max(0) as u64)
                        .wrapping_mul(1_000_000_000)
                        .wrapping_add(nsec.max(0) as u64);
                    if abs {
                        // Absolute deadline in the reported clock domain (base + ns).
                        let target = want.saturating_sub((CLOCK_BASE_SEC as u64) * 1_000_000_000);
                        self.clock_ns = self.clock_ns.max(target);
                    } else {
                        self.clock_ns = self.clock_ns.wrapping_add(want);
                    }
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_GETUID | SYS_GETGID | SYS_GETEUID | SYS_GETEGID | SYS_SETUID | SYS_SETGID => {
                cpu.set_reg(Reg::Rax, 0); // run as root; set*id succeeds
                false
            }
            SYS_CLONE => {
                // clone(flags, ...). CLONE_VM means a shared-address-space *thread*
                // — that's the mt.rs / futex substrate's job (plan D4), not the
                // process model. Report -ENOSYS for it (logged once) so we don't
                // silently fork a process where the guest wanted a thread. Without
                // CLONE_VM it's a process fork: yield to the scheduler.
                //
                // NOTE (task-227): this pre-escalation gap-log fires for *any* CLONE_VM
                // (thread OR vfork/posix_spawn). It is only reached when a clone slips
                // past the deferred scheduler's `is_clone_vm` escalation peek (proc.rs) —
                // i.e. a CLONE_VM-without-CLONE_THREAD clone the peek deliberately leaves
                // deferred. Its predicate is intentionally broader than `is_thread_clone`;
                // we only source the constant from the canonical home (thread.rs), we do
                // NOT tighten which clones it rejects.
                if cpu.reg(Reg::Rdi) & crate::thread::CLONE_VM != 0 {
                    if self.gap_syscalls.insert(SYS_CLONE) {
                        eprintln!("x86jit: clone(CLONE_VM) -> -ENOSYS (threads: use mt substrate) (gap:syscall)");
                    }
                    cpu.set_reg(Reg::Rax, ENOSYS);
                    return false;
                }
                self.pending_fork = true;
                true // yield: the scheduler forks the VM, then resumes this parent
            }
            SYS_FORK | SYS_VFORK => {
                self.pending_fork = true;
                true
            }
            SYS_WAIT4 => {
                self.pending_wait = Some(WaitRequest {
                    pid: cpu.reg(Reg::Rdi) as i64,
                    status_ptr: cpu.reg(Reg::Rsi),
                });
                true // yield: the scheduler runs a pending child and writes status
            }
            SYS_EXECVE => {
                // execve(path, argv[], envp[]): capture the request and hand it to
                // the driver, which replaces the process image and re-enters run().
                // A single-command shell (`sh -c cmd`) exec's directly, no fork.
                let path = read_cstr(vm, cpu.reg(Reg::Rdi));
                let argv = read_cstr_array(vm, cpu.reg(Reg::Rsi));
                let envp = read_cstr_array(vm, cpu.reg(Reg::Rdx));
                self.pending_exec = Some(ExecRequest { path, argv, envp });
                true // leave run(); the driver checks pending_exec vs exit_code
            }
            SYS_CHDIR => {
                // chdir(path). Relative paths already resolve against the rootfs root
                // (rootfs_join ignores the cwd), so this only needs to succeed for a
                // guest that chdirs before opening files (e.g. `httpd -h /`).
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_SCHED_GETAFFINITY => {
                // sched_getaffinity(pid, cpusetsize, mask). Report the real host CPU
                // count so multi-threaded guests (Go GOMAXPROCS, nproc, OpenMP) see true
                // parallelism, instead of the old single-CPU (bit 0) answer. Set the low
                // N bits (bit i in byte i/8) where N is the host count, clamped so it can
                // never exceed the guest-provided cpusetsize (Rsi) buffer. Return the
                // bytes written, preserving the prior `len.max(8)` contract.
                let len = (cpu.reg(Reg::Rsi) as usize).min(128);
                let mask = cpu.reg(Reg::Rdx);
                // Host online CPUs, clamped to [1, 1024] then to the buffer capacity
                // (len*8 bits) so a tiny cpusetsize can never overflow the write.
                let host = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1);
                let ncpus = host.clamp(1, 1024).min(len.saturating_mul(8));
                let mut buf = vec![0u8; len];
                for i in 0..ncpus {
                    buf[i / 8] |= 1 << (i % 8); // CPU i online
                }
                let _ = vm.write_bytes(mask, &buf);
                cpu.set_reg(Reg::Rax, len.max(8) as u64);
                false
            }
            SYS_PRCTL => {
                // prctl(option, ...). Process-control knobs with no analogue in the
                // flat single-process model (PR_SET_NAME, PR_SET_PDEATHSIG, …).
                // Report success so a guest setting them proceeds; none affect us.
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_SOCKET => {
                // socket(domain, type, protocol) → a real host socket. type may carry
                // SOCK_NONBLOCK/SOCK_CLOEXEC; pass through verbatim (host is Linux).
                let domain = cpu.reg(Reg::Rdi) as libc::c_int;
                let ty = cpu.reg(Reg::Rsi) as libc::c_int;
                let proto = cpu.reg(Reg::Rdx) as libc::c_int;
                let r = unsafe { libc::socket(domain, ty, proto) };
                let ret = if r < 0 {
                    host_errno()
                } else {
                    let owned = unsafe { OwnedFd::from_raw_fd(r) };
                    let g = self.fs.alloc_fd();
                    self.fs.fd_table.insert(g, Fd::Socket(Arc::new(owned)));
                    g
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_EPOLL_CREATE1 => {
                // A real host epoll instance (go-caddy P4). `flags` (EPOLL_CLOEXEC) pass
                // through. Go's netpoller registers its sockets here.
                let flags = cpu.reg(Reg::Rdi) as libc::c_int;
                let r = unsafe { libc::epoll_create1(flags) };
                let ret = if r < 0 {
                    host_errno()
                } else {
                    let owned = unsafe { OwnedFd::from_raw_fd(r) };
                    let g = self.fs.alloc_fd();
                    self.fs.fd_table.insert(g, Fd::Epoll(Arc::new(owned)));
                    g
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_EVENTFD2 => {
                // A real host eventfd (go-caddy P4) — Go's netpollBreak wakeup. `flags`
                // (EFD_NONBLOCK/EFD_CLOEXEC) pass through.
                let initval = cpu.reg(Reg::Rdi) as libc::c_uint;
                let flags = cpu.reg(Reg::Rsi) as libc::c_int;
                let r = unsafe { libc::eventfd(initval, flags) };
                let ret = if r < 0 {
                    host_errno()
                } else {
                    let owned = unsafe { OwnedFd::from_raw_fd(r) };
                    let g = self.fs.alloc_fd();
                    self.fs.fd_table.insert(g, Fd::Event(Arc::new(owned)));
                    g
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_EPOLL_CTL => {
                // epoll_ctl(epfd, op, fd, event*). Translate both fds guest→host and
                // forward to the real epoll; the `event.data` u64 is opaque (Go stores a
                // *pollDesc there) and passes through untouched. The guest `epoll_event`
                // is 12 bytes packed (u32 events @0, u64 data @4) — marshal it into the
                // host `libc::epoll_event` explicitly (portable to a 16-byte aarch64 host).
                let epfd_g = cpu.reg(Reg::Rdi);
                let op = cpu.reg(Reg::Rsi) as libc::c_int;
                let fd_g = cpu.reg(Reg::Rdx);
                let event_ptr = cpu.reg(Reg::R10);
                let epfd_h = match self.fs.fd_table.get(&epfd_g) {
                    Some(Fd::Epoll(rc)) => rc.as_raw_fd(),
                    _ => {
                        cpu.set_reg(Reg::Rax, EBADF);
                        return false;
                    }
                };
                // The target must be a host-backed fd; a shim pipe/file/stdio can't enter
                // a host epoll set (task-133) → -EPERM, the kernel's answer for a
                // non-pollable fd.
                let Some(target_h) = self.fs.host_io_fd(fd_g) else {
                    cpu.set_reg(Reg::Rax, EPERM);
                    return false;
                };
                let mut ev: libc::epoll_event = unsafe { std::mem::zeroed() };
                let evp = if event_ptr != 0 {
                    // EPOLL_CTL_DEL may pass NULL; otherwise read the 12-byte guest event.
                    let mut b = [0u8; 12];
                    if vm.read_bytes(event_ptr, &mut b).is_err() {
                        cpu.set_reg(Reg::Rax, EFAULT);
                        return false;
                    }
                    ev.events = u32::from_le_bytes(b[0..4].try_into().unwrap());
                    ev.u64 = u64::from_le_bytes(b[4..12].try_into().unwrap());
                    &mut ev as *mut libc::epoll_event
                } else {
                    std::ptr::null_mut()
                };
                let r = unsafe { libc::epoll_ctl(epfd_h, op, target_h, evp) };
                cpu.set_reg(Reg::Rax, if r < 0 { host_errno() } else { 0 });
                false
            }
            SYS_EPOLL_WAIT | SYS_EPOLL_PWAIT => {
                // Single-process path: block inline (the documented Phase-0 stance for a
                // blocking socket op). Go is a threaded process and never reaches here —
                // `handle_mt` intercepts a blocking `epoll_pwait` and yields it to the
                // driver so it never holds the shim lock while parked (go-caddy P4).
                let epfd_g = cpu.reg(Reg::Rdi);
                let events_ptr = cpu.reg(Reg::Rsi);
                let maxevents = cpu.reg(Reg::Rdx) as i64;
                let timeout_ms = cpu.reg(Reg::R10) as i32;
                let epfd_h = match self.fs.fd_table.get(&epfd_g) {
                    Some(Fd::Epoll(rc)) => rc.as_raw_fd(),
                    _ => {
                        cpu.set_reg(Reg::Rax, EBADF);
                        return false;
                    }
                };
                if maxevents <= 0 {
                    cpu.set_reg(Reg::Rax, EINVAL);
                    return false;
                }
                let ret = do_epoll_wait(epfd_h, vm, events_ptr, maxevents as usize, timeout_ms);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_BIND | SYS_CONNECT => {
                // Both take (fd, sockaddr*, addrlen). The sockaddr layout is identical
                // guest↔host (both Linux x86-64), so the bytes pass through verbatim.
                let fd = cpu.reg(Reg::Rdi);
                let addr = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        if !self.try_fill_scratch(vm, addr, len) {
                            EFAULT
                        } else {
                            let sa = self.scratch.as_ptr() as *const libc::sockaddr;
                            let r = unsafe {
                                if nr == SYS_BIND {
                                    libc::bind(h, sa, len as libc::socklen_t)
                                } else {
                                    libc::connect(h, sa, len as libc::socklen_t)
                                }
                            };
                            if r < 0 {
                                host_errno()
                            } else {
                                0
                            }
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_LISTEN => {
                let fd = cpu.reg(Reg::Rdi);
                let backlog = cpu.reg(Reg::Rsi) as libc::c_int;
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        let r = unsafe { libc::listen(h, backlog) };
                        if r < 0 {
                            host_errno()
                        } else {
                            0
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_ACCEPT | SYS_ACCEPT4 => {
                // accept(fd, sockaddr*, addrlen*) / accept4(..., flags). Blocks the
                // scheduler thread until a peer connects. flags (SOCK_NONBLOCK/
                // SOCK_CLOEXEC) pass through; accept4 with flags==0 is plain accept.
                let fd = cpu.reg(Reg::Rdi);
                let addr = cpu.reg(Reg::Rsi);
                let addrlen_ptr = cpu.reg(Reg::Rdx);
                let flags = if nr == SYS_ACCEPT4 {
                    cpu.reg(Reg::R10) as libc::c_int
                } else {
                    0
                };
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        let mut sa = [0u8; 128];
                        let mut sl = sa.len() as libc::socklen_t;
                        let want_addr = addr != 0;
                        let (aptr, alptr) = if want_addr {
                            (
                                sa.as_mut_ptr() as *mut libc::sockaddr,
                                &mut sl as *mut libc::socklen_t,
                            )
                        } else {
                            (std::ptr::null_mut(), std::ptr::null_mut())
                        };
                        let r = unsafe { libc::accept4(h, aptr, alptr, flags) };
                        if r < 0 {
                            host_errno()
                        } else {
                            write_sockaddr(vm, addr, addrlen_ptr, &sa, sl);
                            let owned = unsafe { OwnedFd::from_raw_fd(r) };
                            let g = self.fs.alloc_fd();
                            self.fs.fd_table.insert(g, Fd::Socket(Arc::new(owned)));
                            g
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SENDTO => {
                // sendto(fd, buf, len, flags, dest_addr, addrlen). A connected TCP socket
                // (openssl s_server/s_client) passes dest_addr == NULL; the datagram form
                // carries a sockaddr, forwarded verbatim (guest↔host layout is identical).
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let flags = cpu.reg(Reg::R10) as libc::c_int;
                let dest = cpu.reg(Reg::R8);
                let addrlen = cpu.reg(Reg::R9) as usize;
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        let mut data = vec![0u8; len];
                        if len > 0 && vm.read_bytes(buf, &mut data).is_err() {
                            EFAULT
                        } else {
                            let sab = if dest != 0 && addrlen != 0 {
                                let mut b = vec![0u8; addrlen.min(128)];
                                if vm.read_bytes(dest, &mut b).is_err() {
                                    return {
                                        cpu.set_reg(Reg::Rax, EFAULT);
                                        false
                                    };
                                }
                                Some(b)
                            } else {
                                None
                            };
                            let (sa_ptr, sa_len) = match &sab {
                                Some(b) => (
                                    b.as_ptr() as *const libc::sockaddr,
                                    b.len() as libc::socklen_t,
                                ),
                                None => (std::ptr::null(), 0),
                            };
                            let n = unsafe {
                                libc::sendto(
                                    h,
                                    data.as_ptr() as *const libc::c_void,
                                    len,
                                    flags,
                                    sa_ptr,
                                    sa_len,
                                )
                            };
                            if n < 0 {
                                host_errno()
                            } else {
                                n as u64
                            }
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_RECVFROM => {
                // recvfrom(fd, buf, len, flags, src_addr, addrlen*). Receive into a host
                // buffer, copy back to the guest, and (if requested) write the peer address.
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let flags = cpu.reg(Reg::R10) as libc::c_int;
                let src = cpu.reg(Reg::R8);
                let addrlen_ptr = cpu.reg(Reg::R9);
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => do_recvfrom(vm, h, buf, len, flags, src, addrlen_ptr),
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SENDMSG => {
                // sendmsg(fd, msghdr*, flags). Gather the iovecs into one buffer (TCP is a
                // byte stream, so coalescing is transparent) and forward, passing the control
                // (cmsg) buffer verbatim — openssl's KTLS probe rides on the control data.
                let fd = cpu.reg(Reg::Rdi);
                let msgp = cpu.reg(Reg::Rsi);
                let flags = cpu.reg(Reg::R10) as libc::c_int;
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        let iov = read_u64(vm, msgp.wrapping_add(16));
                        let iovlen = read_u64(vm, msgp.wrapping_add(24));
                        let control = read_u64(vm, msgp.wrapping_add(32));
                        let controllen = read_u64(vm, msgp.wrapping_add(40)) as usize;
                        let mut data = Vec::new();
                        let mut bad = false;
                        for (base, len) in read_iovecs(vm, iov, iovlen) {
                            if len == 0 {
                                continue;
                            }
                            let mut seg = vec![0u8; len];
                            if vm.read_bytes(base, &mut seg).is_err() {
                                bad = true;
                                break;
                            }
                            data.extend_from_slice(&seg);
                        }
                        let mut cbuf = vec![0u8; controllen];
                        if !bad && controllen > 0 && vm.read_bytes(control, &mut cbuf).is_err() {
                            bad = true;
                        }
                        if bad {
                            EFAULT
                        } else {
                            let mut iovh = libc::iovec {
                                iov_base: data.as_mut_ptr() as *mut libc::c_void,
                                iov_len: data.len(),
                            };
                            let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
                            mh.msg_iov = &mut iovh;
                            mh.msg_iovlen = 1;
                            if controllen > 0 {
                                mh.msg_control = cbuf.as_mut_ptr() as *mut libc::c_void;
                                mh.msg_controllen = controllen;
                            }
                            let n = unsafe { libc::sendmsg(h, &mh, flags) };
                            if n < 0 {
                                host_errno()
                            } else {
                                n as u64
                            }
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_RECVMSG => {
                // recvmsg(fd, msghdr*, flags). Receive into a host buffer, scatter across the
                // guest iovecs, and copy the control (cmsg) buffer + msg_flags back — openssl
                // reads the KTLS record type from the returned control data.
                let fd = cpu.reg(Reg::Rdi);
                let msgp = cpu.reg(Reg::Rsi);
                let flags = cpu.reg(Reg::R10) as libc::c_int;
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => do_recvmsg(vm, h, msgp, flags),
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SETSOCKOPT => {
                // setsockopt(fd, level, optname, optval*, optlen). Forward to the host so
                // SO_REUSEADDR/TCP_NODELAY actually apply, and propagate the host's errno
                // so a guest that checks the return can detect a rejected option (a
                // zero-length optval is a no-op success, as on Linux).
                let fd = cpu.reg(Reg::Rdi);
                let level = cpu.reg(Reg::Rsi) as libc::c_int;
                let name = cpu.reg(Reg::Rdx) as libc::c_int;
                let optval = cpu.reg(Reg::R10);
                let optlen = cpu.reg(Reg::R8) as usize;
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        if optval != 0 && optlen != 0 && !self.try_fill_scratch(vm, optval, optlen)
                        {
                            EFAULT
                        } else if optval != 0 && optlen != 0 {
                            let rc = unsafe {
                                libc::setsockopt(
                                    h,
                                    level,
                                    name,
                                    self.scratch.as_ptr() as *const libc::c_void,
                                    optlen as libc::socklen_t,
                                )
                            };
                            if rc < 0 {
                                host_errno()
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_GETSOCKOPT => {
                // getsockopt(fd, level, optname, optval*, optlen*). Go reads SO_ERROR
                // to check connect completion; forward and copy the result back.
                let fd = cpu.reg(Reg::Rdi);
                let level = cpu.reg(Reg::Rsi) as libc::c_int;
                let name = cpu.reg(Reg::Rdx) as libc::c_int;
                let optval = cpu.reg(Reg::R10);
                let optlen_ptr = cpu.reg(Reg::R8);
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        let mut buf = [0u8; 128];
                        let mut sl = if optlen_ptr != 0 {
                            (read_u32(vm, optlen_ptr) as usize).min(buf.len()) as libc::socklen_t
                        } else {
                            0
                        };
                        let r = unsafe {
                            libc::getsockopt(
                                h,
                                level,
                                name,
                                buf.as_mut_ptr() as *mut libc::c_void,
                                &mut sl,
                            )
                        };
                        if r < 0 {
                            host_errno()
                        } else {
                            let n = (sl as usize).min(buf.len());
                            if optval != 0 {
                                let _ = vm.write_bytes(optval, &buf[..n]);
                            }
                            if optlen_ptr != 0 {
                                let _ = vm.write_bytes(optlen_ptr, &sl.to_le_bytes());
                            }
                            0
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_GETSOCKNAME | SYS_GETPEERNAME => {
                let fd = cpu.reg(Reg::Rdi);
                let addr = cpu.reg(Reg::Rsi);
                let addrlen_ptr = cpu.reg(Reg::Rdx);
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        let mut sa = [0u8; 128];
                        let mut sl = sa.len() as libc::socklen_t;
                        let p = sa.as_mut_ptr() as *mut libc::sockaddr;
                        let r = unsafe {
                            if nr == SYS_GETSOCKNAME {
                                libc::getsockname(h, p, &mut sl)
                            } else {
                                libc::getpeername(h, p, &mut sl)
                            }
                        };
                        if r < 0 {
                            host_errno()
                        } else {
                            write_sockaddr(vm, addr, addrlen_ptr, &sa, sl);
                            0
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SHUTDOWN => {
                let fd = cpu.reg(Reg::Rdi);
                let how = cpu.reg(Reg::Rsi) as libc::c_int;
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => {
                        let r = unsafe { libc::shutdown(h, how) };
                        if r < 0 {
                            host_errno()
                        } else {
                            0
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_SELECT | SYS_PSELECT6 => {
                // select/pselect6(nfds, rfds, wfds, efds, timeout, [sigmask]). Forward the
                // host-backed fds (sockets/eventfds/epoll) to a real host `select` so a guest
                // that waits for a connection actually blocks; non-host fds (files/stdio) are
                // always reported ready (matching the SYS_POLL stance). openssl's accept loop
                // selects the listening socket — this must genuinely block (task-215).
                let nfds = (cpu.reg(Reg::Rdi) as i64).clamp(0, 1024) as usize;
                let set_ptrs = [
                    cpu.reg(Reg::Rsi), // read
                    cpu.reg(Reg::Rdx), // write
                    cpu.reg(Reg::R10), // except
                ];
                let tp = cpu.reg(Reg::R8);
                // Read the three guest fd_sets (128 bytes = 1024 bits each).
                let mut guest_in = [[0u8; 128]; 3];
                for (s, p) in set_ptrs.iter().enumerate() {
                    if *p != 0 {
                        let _ = vm.read_bytes(*p, &mut guest_in[s]);
                    }
                }
                let bit = |set: &[u8; 128], i: usize| (set[i / 8] >> (i % 8)) & 1 != 0;
                let mut out = [[0u8; 128]; 3];
                let mut ready = 0u64;
                // Build host fd_sets for the host-backed fds; mark non-host fds ready now.
                let mut hset: [libc::fd_set; 3] = unsafe { std::mem::zeroed() };
                for h in hset.iter_mut() {
                    unsafe { libc::FD_ZERO(h) };
                }
                let mut maxfd = -1i32;
                let mut host_fds: Vec<(usize, usize, i32)> = Vec::new(); // (set, guest_fd, host_fd)
                for s in 0..3 {
                    if set_ptrs[s] == 0 {
                        continue;
                    }
                    for g in 0..nfds {
                        if !bit(&guest_in[s], g) {
                            continue;
                        }
                        match self.fs.host_io_fd(g as u64) {
                            Some(h) => {
                                unsafe { libc::FD_SET(h, &mut hset[s]) };
                                if h > maxfd {
                                    maxfd = h;
                                }
                                host_fds.push((s, g, h));
                            }
                            None => {
                                // Non-host fd: always ready.
                                out[s][g / 8] |= 1 << (g % 8);
                                ready += 1;
                            }
                        }
                    }
                }
                // Host select over the host-backed fds. The blocking decision keys off
                // whether the set actually contains a host-backed fd — *not* off `ready`,
                // which the always-ready non-host fds inflate. If we collapsed to a
                // zero-timeout poll merely because some non-host fd was ready, a mixed set
                // (e.g. a listening socket paired with a self-pipe) would never wait for a
                // connection: the host `select` returns instantly and the guest spins at
                // 100% CPU. So:
                //   - at least one host fd → run `select` with the guest's real timeout
                //     (NULL = block forever) so the sockets genuinely block, then union
                //     the always-ready non-host fds into the result below;
                //   - purely non-host set → keep the immediate zero-timeout poll (matches
                //     the SYS_POLL stance: files/stdio are always reported ready).
                let has_host_fd = !host_fds.is_empty();
                let mut tvbuf = libc::timeval {
                    tv_sec: 0,
                    tv_usec: 0,
                };
                let tvptr: *mut libc::timeval = if !has_host_fd {
                    &mut tvbuf // immediate return: no host fd to block on
                } else if tp != 0 {
                    let sec = read_u64(vm, tp) as i64;
                    let frac = read_u64(vm, tp.wrapping_add(8)) as i64;
                    // pselect6 timeout is timespec (ns); select is timeval (µs).
                    tvbuf.tv_sec = sec;
                    tvbuf.tv_usec = if nr == SYS_PSELECT6 {
                        frac / 1000
                    } else {
                        frac
                    };
                    &mut tvbuf
                } else {
                    std::ptr::null_mut()
                };
                let rp = if set_ptrs[0] != 0 {
                    &mut hset[0] as *mut libc::fd_set
                } else {
                    std::ptr::null_mut()
                };
                let wp = if set_ptrs[1] != 0 {
                    &mut hset[1] as *mut libc::fd_set
                } else {
                    std::ptr::null_mut()
                };
                let epp = if set_ptrs[2] != 0 {
                    &mut hset[2] as *mut libc::fd_set
                } else {
                    std::ptr::null_mut()
                };
                let r = unsafe { libc::select(maxfd + 1, rp, wp, epp, tvptr) };
                if r < 0 {
                    cpu.set_reg(Reg::Rax, host_errno());
                    return false;
                }
                for (s, g, h) in host_fds {
                    if unsafe { libc::FD_ISSET(h, &hset[s]) } {
                        out[s][g / 8] |= 1 << (g % 8);
                        ready += 1;
                    }
                }
                // Write the result sets back (kernel overwrites them in place).
                for (s, p) in set_ptrs.iter().enumerate() {
                    if *p != 0 {
                        let _ = vm.write_bytes(*p, &out[s]);
                    }
                }
                cpu.set_reg(Reg::Rax, ready);
                false
            }
            SYS_POLL => {
                // poll(fds, nfds, timeout). No real readiness model: report every
                // valid fd ready for whatever it requested (stdout is always
                // writable; a file/stdin read that follows returns data or EOF).
                // Non-blocking and deterministic. pollfd = {i32 fd; i16 events; i16
                // revents} — 8 bytes; events at +4, revents at +6.
                let fds = cpu.reg(Reg::Rdi);
                let nfds = cpu.reg(Reg::Rsi).min(1024);
                let mut ready = 0u64;
                for i in 0..nfds {
                    let ent = fds.wrapping_add(i * 8);
                    let word = read_u64(vm, ent);
                    let fd = word as i32;
                    let events = (word >> 32) as u16;
                    let revents = if fd >= 0 { events } else { 0 };
                    if revents != 0 {
                        ready += 1;
                    }
                    let _ = vm.write_bytes(ent.wrapping_add(6), &revents.to_le_bytes());
                }
                cpu.set_reg(Reg::Rax, ready);
                false
            }
            SYS_STATFS | SYS_FSTATFS => {
                // Synthetic filesystem stats — a plausible ext-like fs so a guest
                // sizing I/O buffers or checking free space gets sane, non-zero
                // values. `buf` is the second argument for both calls.
                write_statfs(vm, cpu.reg(Reg::Rsi));
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_TGKILL => {
                // tgkill(tgid, tid, sig). No signal machinery; a fatal self-signal
                // (the abort()/raise path) terminates the process with 128+sig, the
                // kernel's default disposition. Other signals are dropped (0).
                let sig = cpu.reg(Reg::Rdx) as i32;
                const FATAL: [i32; 6] = [3, 4, 6, 8, 9, 11]; // QUIT ILL ABRT FPE KILL SEGV
                if FATAL.contains(&sig) {
                    self.exit_code = Some(128 + sig);
                    return true;
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_EXIT | SYS_EXIT_GROUP => {
                self.exit_code = Some(cpu.reg(Reg::Rdi) as i32);
                true
            }
            other => {
                // A scripted answer wins (test oracle); otherwise degrade
                // gracefully to -ENOSYS so the guest can take a fallback path
                // (e.g. sendfile -> read/write loop), and log the gap once — the
                // syscall analogue of Exit::UnknownInstruction (OCI gap pipeline).
                let ret = self.scripted.get(other).unwrap_or_else(|| {
                    if self.gap_syscalls.insert(other) {
                        eprintln!("x86jit: unhandled syscall {other} -> -ENOSYS (gap:syscall)");
                    }
                    ENOSYS
                });
                cpu.set_reg(Reg::Rax, ret);
                false
            }
        }
    }

    /// Handle one i386 `int 0x80` syscall (`CpuMode::Compat32`). The ABI differs from
    /// x86-64 on three axes, all handled here so the long-mode [`handle`](Self::handle)
    /// stays untouched:
    ///
    /// - **numbering** — a separate `SYS32_*` table (`exit`=1, `write`=4, `brk`=45,
    ///   `mmap2`=192, …);
    /// - **argument registers** — number in `EAX`, args in `EBX/ECX/EDX/ESI/EDI/EBP`
    ///   (the low 32 bits of the corresponding 64-bit GPRs), each zero-extended;
    /// - **struct widths** — pointers are 4 bytes (`iovec` is 8 bytes/entry, not 16),
    ///   and `mmap2` takes its offset in 4 KiB pages, not bytes.
    ///
    /// The byte-level plumbing (`do_write`, the brk/mmap arena, TLS base) is shared;
    /// only the decode is 32-bit. Anything not needed by a static i386 hello is
    /// rejected loudly with its number (§17.7), the syscall analogue of a lift gap.
    fn handle_i386(&mut self, cpu: &mut Vcpu, vm: &Vm) -> bool {
        // i386 int-0x80 register file, zero-extended to guest addresses.
        let nr = cpu.reg(Reg::Rax) & 0xffff_ffff;
        let ebx = cpu.reg(Reg::Rbx) & 0xffff_ffff;
        let ecx = cpu.reg(Reg::Rcx) & 0xffff_ffff;
        let edx = cpu.reg(Reg::Rdx) & 0xffff_ffff;
        let esi = cpu.reg(Reg::Rsi) & 0xffff_ffff;
        let edi = cpu.reg(Reg::Rdi) & 0xffff_ffff;
        let ebp = cpu.reg(Reg::Rbp) & 0xffff_ffff;

        // i386 returns in EAX, so mask every result into the low 32 bits — a negative
        // errno stays a small-negative i32 the guest reads correctly.
        let set_eax = |cpu: &mut Vcpu, val: u64| cpu.set_reg(Reg::Rax, val & 0xffff_ffff);

        match nr {
            SYS32_EXIT | SYS32_EXIT_GROUP => {
                self.exit_code = Some(ebx as i32);
                true
            }
            SYS32_WRITE => {
                let ret = self.do_write(vm, ebx, ecx, edx as usize);
                set_eax(cpu, ret);
                false
            }
            SYS32_READ => {
                let ret = self.do_read(vm, ebx, ecx, edx as usize);
                set_eax(cpu, ret);
                false
            }
            SYS32_OPEN => {
                let ret = self.do_open(vm, ebx, ecx);
                set_eax(cpu, ret);
                false
            }
            SYS32_CLOSE => {
                let ret = if self.release(ebx) { 0 } else { EBADF };
                set_eax(cpu, ret);
                false
            }
            SYS32_WRITEV => {
                // writev(fd=EBX, iov=ECX, iovcnt=EDX). i386 iovec is
                // { u32 iov_base; u32 iov_len } — 8 bytes/entry (half the x86-64 width).
                let iov = ecx;
                let mut total = 0u64;
                for i in 0..edx {
                    let base = read_u32(vm, iov + i * 8) as u64;
                    let len = read_u32(vm, iov + i * 8 + 4) as usize;
                    if len == 0 {
                        continue;
                    }
                    let n = self.do_write(vm, ebx, base, len);
                    if (n as i32) < 0 {
                        if total == 0 {
                            total = n;
                        }
                        break;
                    }
                    total += n;
                }
                set_eax(cpu, total);
                false
            }
            SYS32_BRK => {
                // brk(0) queries; brk(addr) grows within the limit (TASK-93).
                if ebx != 0 && ebx >= self.brk && ebx <= self.brk_limit {
                    self.brk = ebx;
                }
                set_eax(cpu, self.brk);
                false
            }
            SYS32_MMAP2 => {
                // mmap2(addr, len, prot, flags, fd, pgoff): identical to the x86-64
                // arena logic, but the offset is in 4 KiB pages (shift by 12) and the
                // args come from EBX..EBP. Anonymous only (file-backed i386 mmap is a
                // dynamic-linking concern, deferred).
                const MAP_FIXED: u64 = 0x10;
                let addr = ebx;
                let len = ecx;
                let flags = esi;
                let fd = edi as u32 as i32;
                let _pgoff = ebp;
                let target = if flags & MAP_FIXED != 0 {
                    addr
                } else {
                    let aligned = (len + 0xfff) & !0xfff;
                    match self.arena_alloc(aligned, vm) {
                        Some(a) => a,
                        None => {
                            set_eax(cpu, ENOMEM);
                            return false;
                        }
                    }
                };
                if fd >= 0 {
                    // File-backed i386 mmap2 belongs to dynamic linking — refuse loudly
                    // rather than silently mishandle it (§17.7).
                    if self.gap_syscalls.insert(SYS32_MMAP2) {
                        eprintln!(
                            "x86jit: i386 file-backed mmap2 (fd={fd}) unsupported (gap:syscall-i386-mmap2)"
                        );
                    }
                    set_eax(cpu, EINVAL);
                    return false;
                }
                if flags & MAP_FIXED != 0 && self.try_resize_scratch(len as usize) {
                    let _ = vm.write_bytes(target, &self.scratch);
                }
                set_eax(cpu, target);
                false
            }
            SYS32_MUNMAP => {
                // Reclaim the anonymous span into the arena, like the x86-64 path (task-124).
                self.arena_free(ebx, ecx);
                set_eax(cpu, 0);
                false
            }
            SYS32_MPROTECT => {
                // No-op: the flat model has no page protection.
                set_eax(cpu, 0);
                false
            }
            SYS32_SET_THREAD_AREA => {
                // i386 TLS: the guest passes a `struct user_desc *` in EBX; record its
                // base_addr as the GS base (the core adds it for GS-prefixed accesses,
                // §17.5) and hand back an entry_number so glibc/musl can build the GS
                // selector. A minimal deliberate shim — no real GDT (TASK-199).
                let ud = ebx;
                let entry_number = read_u32(vm, ud) as i32;
                let base_addr = read_u32(vm, ud + 4) as u64;
                cpu.set_reg(Reg::GsBase, base_addr);
                // -1 means "allocate one": report the conventional first i386 TLS entry.
                let allocated = if entry_number == -1 {
                    6
                } else {
                    entry_number as u32
                };
                let _ = vm.write_bytes(ud, &allocated.to_le_bytes());
                set_eax(cpu, 0);
                false
            }
            SYS32_SET_TID_ADDRESS => {
                set_eax(cpu, 1); // pretend tid 1
                false
            }
            SYS32_UNAME => {
                // `struct old_utsname` / `new_utsname`: 6 × char[65], identical bytes
                // to the x86-64 layout apart from the machine string.
                const FIELD: usize = 65;
                let fields: [&[u8]; 6] = [
                    b"Linux",
                    b"x86jit",
                    b"6.1.0",
                    b"#1 SMP x86jit",
                    b"i686",
                    b"(none)",
                ];
                let mut uts = [0u8; FIELD * 6];
                for (i, f) in fields.iter().enumerate() {
                    let off = i * FIELD;
                    let n = f.len().min(FIELD - 1);
                    uts[off..off + n].copy_from_slice(&f[..n]);
                }
                let ret = match vm.write_bytes(ebx, &uts) {
                    Ok(()) => 0,
                    Err(_) => EFAULT,
                };
                set_eax(cpu, ret);
                false
            }
            SYS32_READLINK => {
                set_eax(cpu, self.do_readlink(vm, ebx, ecx, edx));
                false
            }
            SYS32_READLINKAT => {
                // readlinkat(dirfd, path, buf, bufsiz): path/buf/size shift by one arg.
                set_eax(cpu, self.do_readlink(vm, ecx, edx, esi));
                false
            }
            SYS32_GETRANDOM => {
                // Same entropy source as the x86-64 path (task-128).
                let len = ecx as usize;
                if self.fill_scratch_entropy(len) {
                    let _ = vm.write_bytes(ebx, &self.scratch);
                }
                set_eax(cpu, ecx);
                false
            }
            other => {
                let ret = self.scripted.get(other).unwrap_or_else(|| {
                    if self.gap_syscalls.insert(other) {
                        eprintln!(
                            "x86jit: unhandled i386 syscall {other} -> -ENOSYS (gap:syscall-i386)"
                        );
                    }
                    ENOSYS
                });
                set_eax(cpu, ret);
                false
            }
        }
    }

    /// `readlink`/`readlinkat` of `/proc/self/exe` → `exe_path`; anything else
    /// `-ENOENT`. Shared by the i386 arms (the byte plumbing is ABI-neutral).
    fn do_readlink(&mut self, vm: &Vm, path: u64, buf: u64, bufsiz: u64) -> u64 {
        let name = read_cstr(vm, path);
        if name == b"/proc/self/exe" && !self.exe_path.is_empty() {
            let out = &self.exe_path[..self.exe_path.len().min(bufsiz as usize)];
            match vm.write_bytes(buf, out) {
                Ok(()) => out.len() as u64,
                Err(_) => EFAULT,
            }
        } else {
            ENOENT
        }
    }

    /// Threaded-driver syscall entry (P2.3+): the multithread-aware sibling of
    /// [`handle`](Self::handle). It intercepts the operations that must not run under
    /// the shim lock or that need per-thread answers — blocking `futex`,
    /// `clone(CLONE_VM)`, and the per-thread identity/lifecycle calls
    /// (`gettid`/`set_tid_address`/`exit`) — returning them **by value** so the driver
    /// services them after the guard drops (lock order: shim → futex). Every other
    /// syscall routes through the single-process handler unchanged, so the differential
    /// corpus keeps `handle`'s exact semantics as its oracle.
    pub fn handle_mt(&mut self, cpu: &mut Vcpu, vm: &Vm, ctx: &mut ThreadCtx) -> SyscallOutcome {
        match cpu.reg(Reg::Rax) {
            SYS_FUTEX => self.futex_mt(cpu, vm),
            // Per-thread identity: answer from the caller's `ThreadCtx`, not the shared
            // shim, and leave `handle`'s single-process `gettid`→pid path untouched.
            SYS_GETTID => {
                cpu.set_reg(Reg::Rax, ctx.tid);
                SyscallOutcome::Continue
            }
            SYS_SET_TID_ADDRESS => {
                // Records this thread's clear_tid (the pthread_join handshake address);
                // musl's main thread sets it at startup, so this also gives the main
                // thread CHILD_CLEARTID semantics for free.
                ctx.clear_tid = cpu.reg(Reg::Rdi);
                cpu.set_reg(Reg::Rax, ctx.tid);
                SyscallOutcome::Continue
            }
            // Per-thread robust-futex list (task-122): record the head/len in this
            // thread's `ThreadCtx` so the driver can walk it on exit (setting
            // FUTEX_OWNER_DIED + waking a waiter per held mutex). The single-threaded
            // `handle` keeps its no-op; only a threaded process has siblings to unblock.
            SYS_SET_ROBUST_LIST => {
                // set_robust_list(head, len): the kernel only checks len == sizeof(struct
                // robust_list_head) (24). Accept any len (record it for get_robust_list),
                // but store the head only when the len is the canonical size, matching the
                // kernel's -EINVAL for a bogus len.
                let head = cpu.reg(Reg::Rdi);
                let len = cpu.reg(Reg::Rsi);
                if len != ROBUST_LIST_HEAD_SIZE {
                    cpu.set_reg(Reg::Rax, (-22i64) as u64); // -EINVAL
                } else {
                    ctx.robust_list_head = head;
                    ctx.robust_list_len = len;
                    cpu.set_reg(Reg::Rax, 0);
                }
                SyscallOutcome::Continue
            }
            SYS_GET_ROBUST_LIST => {
                // get_robust_list(pid, head_ptr, len_ptr): write this thread's stored head
                // and len back (pid 0 = the caller; we only model the caller's own list).
                let head_ptr = cpu.reg(Reg::Rsi);
                let len_ptr = cpu.reg(Reg::Rdx);
                let ret = if vm
                    .write_bytes(head_ptr, &ctx.robust_list_head.to_le_bytes())
                    .is_err()
                    || vm
                        .write_bytes(len_ptr, &ctx.robust_list_len.to_le_bytes())
                        .is_err()
                {
                    EFAULT
                } else {
                    0
                };
                cpu.set_reg(Reg::Rax, ret);
                SyscallOutcome::Continue
            }
            // `exit(2)` ends just this thread; `exit_group` (via `handle`) ends the
            // process. Intercept `exit` here so it never sets the shared `exit_code`.
            SYS_EXIT => SyscallOutcome::ThreadExit(cpu.reg(Reg::Rdi) as i32),
            // Route to `clone_thread` ONLY for a real *thread* clone (CLONE_VM|CLONE_THREAD)
            // — the same canonical test the deferred scheduler escalates on (task-227). A
            // `vfork`/`posix_spawn` (CLONE_VM|CLONE_VFORK, no CLONE_THREAD) reaching an
            // already-threaded process must NOT spawn a sibling host thread over the shared
            // address space (its `execve` would corrupt the whole process); it falls through
            // to `fork_eagain` below with the plain forks.
            SYS_CLONE if crate::thread::is_thread_clone(cpu.reg(Reg::Rax), cpu.reg(Reg::Rdi)) => {
                self.clone_thread(cpu, vm)
            }
            // A process fork (fork/vfork, or a clone that is not a real thread clone —
            // e.g. vfork/posix_spawn's CLONE_VM|CLONE_VFORK) is not modeled for a threaded
            // process — Linux fork only duplicates the calling thread. Return fork's real
            // resource errno (-EAGAIN), which every runtime handles, rather than lying or
            // crashing (P2.8).
            SYS_FORK | SYS_VFORK | SYS_CLONE => self.fork_eagain(cpu),
            // In mt mode the virtual clock no longer advances on a `nanosleep`, so a
            // sleep-until-deadline loop would spin hot; the driver performs a real,
            // interruptible sleep instead. `sched_yield` yields the host thread.
            SYS_NANOSLEEP | SYS_CLOCK_NANOSLEEP => self.sleep_mt(cpu, vm),
            SYS_SCHED_YIELD => {
                cpu.set_reg(Reg::Rax, 0);
                SyscallOutcome::Yield
            }
            // Per-thread signal state: Go installs a distinct alt stack and mask per M,
            // so these can't share the process-wide shim fields — they live in the
            // caller's `ThreadCtx`. No delivery (P3); recorded and read back only.
            SYS_SIGALTSTACK => {
                let ret = do_sigaltstack(cpu, vm, &mut ctx.altstack);
                cpu.set_reg(Reg::Rax, ret);
                SyscallOutcome::Continue
            }
            SYS_RT_SIGPROCMASK => {
                let ret = do_sigprocmask(cpu, vm, &mut ctx.sigmask);
                cpu.set_reg(Reg::Rax, ret);
                SyscallOutcome::Continue
            }
            SYS_EPOLL_WAIT | SYS_EPOLL_PWAIT => self.epoll_wait_mt(cpu, vm),
            // Blocking fd I/O (task-125): serve inline when data is ready (or the fd is a
            // file that never blocks — delegated), else yield a `Blocking*` outcome the
            // driver services outside the shim lock, exactly like `epoll_wait_mt`.
            SYS_READ => self.read_mt(cpu, vm),
            SYS_READV => self.readv_mt(cpu, vm),
            SYS_ACCEPT | SYS_ACCEPT4 => self.accept_mt(cpu, vm),
            // task-233: a blocking-mode `recvfrom`/`recvmsg` on an empty socket must not issue
            // its host syscall under the shim lock (same deadlock class as read/accept). Serve
            // inline when the fd is nonblocking or already readable (or not a host socket);
            // otherwise yield `BlockingRecv` for the driver to park + complete outside the lock.
            SYS_RECVFROM => self.recvfrom_mt(cpu, vm),
            SYS_RECVMSG => self.recvmsg_mt(cpu, vm),
            // Everything else (including `execve`, `wait4`) routes through the
            // single-process handler.
            _ => self.delegate_mt(cpu, vm),
        }
    }

    /// A process fork from a threaded process → guest-visible `-EAGAIN` (fork's real
    /// resource-exhaustion errno), logged once. A guest that retries in a loop will
    /// spin — acceptable, and no worse than a hang it chose (P2.8).
    fn fork_eagain(&mut self, cpu: &mut Vcpu) -> SyscallOutcome {
        const EAGAIN: u64 = (-11i64) as u64;
        if self.gap_syscalls.insert(cpu.reg(Reg::Rax)) {
            eprintln!("x86jit: fork in a threaded process -> -EAGAIN (gap:syscall)");
        }
        cpu.set_reg(Reg::Rax, EAGAIN);
        SyscallOutcome::Continue
    }

    /// Route a non-intercepted syscall through the single-process [`handle`](Self::handle)
    /// and translate its yield-bool into the threaded vocabulary: a yield with an exit
    /// code is `exit_group`; any other yield (execve/wait/blocking pipe) has no honest
    /// errno for a threaded process, so it surfaces as `Unsupported` naming the op — a
    /// `ProcError` for the driver, never a host panic (P2.8).
    fn delegate_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        if self.handle(cpu, vm) {
            if let Some(code) = self.exit_code {
                return SyscallOutcome::ProcessExit(code);
            }
            // Name the offending op from the yield it parked (execve replaces the image
            // and would kill siblings; wait4/blocking-read have no thread-local answer).
            let what = if self.pending_exec.is_some() {
                "execve"
            } else if self.pending_wait.is_some() {
                "wait4"
            } else if self.pending_read.is_some() {
                "blocking pipe read"
            } else {
                "a blocking/multi-process syscall"
            };
            SyscallOutcome::Unsupported { what }
        } else {
            SyscallOutcome::Continue
        }
    }

    /// The `futex` intercept: the WAIT/WAKE family is returned by value for the driver
    /// to service against `ThreadShared` after the shim guard drops (lock order: shim →
    /// futex). Handles the four glibc/musl pthreads use:
    ///
    /// - `FUTEX_WAIT` (0) / `FUTEX_WAKE` (1): plain, unified as bitmask `0xffff_ffff`
    ///   (match-any). `WAIT`'s R10 `timespec` is a *relative* bound.
    /// - `FUTEX_WAIT_BITSET` (9) / `FUTEX_WAKE_BITSET` (10) (task-121): the `val3`
    ///   bitmask is in R9; a `WAKE_BITSET` releases only queued `WAIT_BITSET` waiters
    ///   whose stored bitmask ANDs nonzero with it. `WAIT_BITSET`'s R10 timeout is an
    ///   *absolute* deadline (converted to relative here against the shared virtual
    ///   clock). The `FUTEX_CLOCK_REALTIME` (0x100) flag is stripped by `CMD_MASK` and
    ///   otherwise ignored — our `clock_gettime` collapses every clock id onto one virtual
    ///   axis, so monotonic and realtime deadlines rebase identically; see
    ///   [`abs_deadline_to_rel`](Self::abs_deadline_to_rel) for the single-clock model.
    fn futex_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        const FUTEX_CMD_MASK: u64 = 0x7f; // strip PRIVATE / CLOCK_REALTIME flags
        const FUTEX_WAIT: u64 = 0;
        const FUTEX_WAKE: u64 = 1;
        const FUTEX_WAIT_BITSET: u64 = 9;
        const FUTEX_WAKE_BITSET: u64 = 10;
        // Plain WAIT/WAKE behave as a match-any bitmask, unifying them with the BITSET
        // ops so the queue always matches on a bitmask (task-121).
        const MATCH_ANY: u32 = 0xffff_ffff;
        let opword = cpu.reg(Reg::Rsi);
        let op = opword & FUTEX_CMD_MASK;
        match op {
            FUTEX_WAIT => {
                let uaddr = cpu.reg(Reg::Rdi);
                let val = cpu.reg(Reg::Rdx) as u32;
                // 4th arg (R10): a *relative* `timespec { i64 sec, i64 nsec }`, or null
                // for an indefinite wait (Go's `futexsleep` passes both forms).
                let ts = cpu.reg(Reg::R10);
                let timeout = if ts != 0 {
                    let sec = read_u64(vm, ts);
                    let nsec = (read_u64(vm, ts.wrapping_add(8)) % 1_000_000_000) as u32;
                    Some(Duration::new(sec, nsec))
                } else {
                    None
                };
                SyscallOutcome::FutexWait {
                    uaddr,
                    val,
                    timeout,
                    bitmask: MATCH_ANY,
                }
            }
            FUTEX_WAKE => SyscallOutcome::FutexWake {
                uaddr: cpu.reg(Reg::Rdi),
                count: cpu.reg(Reg::Rdx),
                bitmask: MATCH_ANY,
            },
            FUTEX_WAIT_BITSET => {
                let uaddr = cpu.reg(Reg::Rdi);
                let val = cpu.reg(Reg::Rdx) as u32;
                // val3 (R9) is the bitmask. A zero bitmask is invalid (-EINVAL); glibc
                // never passes it, but a guest could.
                let bitmask = cpu.reg(Reg::R9) as u32;
                if bitmask == 0 {
                    cpu.set_reg(Reg::Rax, (-22i64) as u64); // -EINVAL
                    return SyscallOutcome::Continue;
                }
                // R10 is an *absolute* deadline `timespec`, not a relative one; null =
                // indefinite. Convert to a relative bound against the shared virtual
                // clock (a past deadline → immediate -ETIMEDOUT).
                let ts = cpu.reg(Reg::R10);
                if ts != 0 {
                    let sec = read_u64(vm, ts);
                    let nsec = read_u64(vm, ts.wrapping_add(8));
                    match self.abs_deadline_to_rel(sec, nsec) {
                        Some(rel) => SyscallOutcome::FutexWait {
                            uaddr,
                            val,
                            timeout: Some(rel),
                            bitmask,
                        },
                        None => {
                            // Deadline already in the past → immediate -ETIMEDOUT, but
                            // only if the guest word still matches (a mismatch is
                            // -EAGAIN, which the driver's value re-check decides — so we
                            // still route through FutexWait with a zero timeout to keep
                            // that linearization point in one place).
                            SyscallOutcome::FutexWait {
                                uaddr,
                                val,
                                timeout: Some(Duration::ZERO),
                                bitmask,
                            }
                        }
                    }
                } else {
                    SyscallOutcome::FutexWait {
                        uaddr,
                        val,
                        timeout: None,
                        bitmask,
                    }
                }
            }
            FUTEX_WAKE_BITSET => {
                let bitmask = cpu.reg(Reg::R9) as u32;
                if bitmask == 0 {
                    cpu.set_reg(Reg::Rax, (-22i64) as u64); // -EINVAL
                    return SyscallOutcome::Continue;
                }
                SyscallOutcome::FutexWake {
                    uaddr: cpu.reg(Reg::Rdi),
                    count: cpu.reg(Reg::Rdx),
                    bitmask,
                }
            }
            _ => {
                // REQUEUE / WAKE_OP / PI-ops / …: not yet modeled. A WAIT-class op that
                // returned instant success would spin a glibc guest, so log the gap
                // instead of silently succeeding (task-121). Non-WAIT ops (WAKE_OP,
                // REQUEUE) degrade to success like the single-threaded shim.
                if self.gap_syscalls.insert(SYS_FUTEX) {
                    eprintln!("x86jit: futex op {op} unsupported -> 0 (gap:syscall)");
                }
                cpu.set_reg(Reg::Rax, 0);
                SyscallOutcome::Continue
            }
        }
    }

    /// Convert a `FUTEX_WAIT_BITSET` **absolute** deadline `(sec, nsec)` into a relative
    /// `Duration` against the shared virtual clock (task-121). Returns `None` when the
    /// deadline is already in the past (the caller yields an immediate `-ETIMEDOUT`).
    ///
    /// **Clock simplification (documented).** The rest of the mt path runs on one virtual
    /// monotonic clock (VCLK, decision-6): `now_ns` counts nanoseconds since process start,
    /// but our `clock_gettime`/`gettimeofday` report `CLOCK_BASE_SEC + now_ns` (`tick_clock`)
    /// for **every** clock id — the shim does not distinguish `CLOCK_MONOTONIC` from
    /// `CLOCK_REALTIME`. A guest builds its absolute deadline from that reported value
    /// (glibc's `pthread_cond_timedwait`/`sem_timedwait` call `clock_gettime` then add a
    /// delta), so BOTH `CLOCK_MONOTONIC` (the glibc ≥2.30 default, no `FUTEX_CLOCK_REALTIME`
    /// flag) and `CLOCK_REALTIME` deadlines live in the `CLOCK_BASE_SEC`-based domain.
    /// We therefore drop the base **unconditionally** to land on the `now_ns` axis, then
    /// `rel = deadline_mono - now_ns` — regardless of which clock the guest named. (An
    /// earlier version rebased only the realtime flag, leaving a monotonic deadline
    /// ~`CLOCK_BASE_SEC` ≈ 54 years in the future → an effectively indefinite wait that
    /// never timed out. task-121 review fix.)
    ///
    /// This keeps the relative wait non-negative and bounded. A real monotonic deadline
    /// from our clock is always ≥ `CLOCK_BASE_SEC` (since `clock_gettime` returns
    /// `base + ns`, `ns ≥ 0`), so the `saturating_sub` never spuriously zeroes a genuine
    /// deadline; a malformed sub-base deadline the guest never derived from our clock
    /// degrades to an immediate `-ETIMEDOUT`, which is harmless.
    fn abs_deadline_to_rel(&mut self, sec: u64, nsec: u64) -> Option<Duration> {
        let nsec = nsec.min(999_999_999);
        let deadline_ns = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
        // Our `clock_gettime` reports `CLOCK_BASE_SEC + now_ns` for every clock id, so the
        // guest's absolute deadline is in the base-offset domain whether it named
        // CLOCK_MONOTONIC or CLOCK_REALTIME; drop the base to land on the `now_ns` axis.
        let deadline_mono = deadline_ns.saturating_sub((CLOCK_BASE_SEC as u64) * 1_000_000_000);
        let now = self.now_ns();
        if deadline_mono <= now {
            None
        } else {
            Some(Duration::from_nanos(deadline_mono - now))
        }
    }

    /// The `clone(CLONE_VM)` intercept: build the child `CpuState` per the clone ABI,
    /// perform the PARENT/CHILD_SETTID guest-memory writes, set the parent's `Rax` to
    /// the new tid, and hand the finished child to the driver via `Spawn`. Runs under
    /// the shim lock, so the `next_tid` bump is race-free.
    fn clone_thread(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        const CLONE_SETTLS: u64 = 0x0008_0000;
        const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
        const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
        const CLONE_CHILD_SETTID: u64 = 0x0100_0000;
        let flags = cpu.reg(Reg::Rdi);
        let stack = cpu.reg(Reg::Rsi);
        let ptid = cpu.reg(Reg::Rdx);
        let ctid = cpu.reg(Reg::R10);
        let tls = cpu.reg(Reg::R8);
        let tid = self.next_tid;
        self.next_tid += 1;
        // First thread: flip to mt mode and seed the shared virtual clock from the
        // single-threaded value so a threaded program's time never jumps backward across
        // the switch (VCLK, decision-6). From here `now_ns` ticks `mt_clock`.
        if !self.threaded {
            self.threaded = true;
            self.mt_clock.seed(self.clock_ns);
        }

        let mut child = cpu.cpu.clone();
        child.gpr[0] = 0; // the child returns 0 from clone (RAX)
        child.gpr[4] = stack; // RSP
        if flags & CLONE_SETTLS != 0 {
            child.fs_base = tls;
        }
        if flags & CLONE_PARENT_SETTID != 0 {
            let _ = vm.write_bytes(ptid, &(tid as u32).to_le_bytes());
        }
        if flags & CLONE_CHILD_SETTID != 0 {
            let _ = vm.write_bytes(ctid, &(tid as u32).to_le_bytes());
        }
        let clear_tid = if flags & CLONE_CHILD_CLEARTID != 0 {
            ctid
        } else {
            0
        };

        cpu.set_reg(Reg::Rax, tid); // the parent gets the child tid
        SyscallOutcome::Spawn {
            child_cpu: Box::new(child),
            child_tid: tid,
            clear_tid,
        }
    }

    /// The mt-mode `nanosleep`/`clock_nanosleep` intercept: compute the requested
    /// duration (relative, or an absolute deadline minus now) and hand it to the driver
    /// as [`SyscallOutcome::Sleep`] for a real, interruptible wait. `Rax = 0` (we always
    /// complete the sleep, so the `rem` timespec is left untouched).
    fn sleep_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        const TIMER_ABSTIME: u64 = 1;
        let nr = cpu.reg(Reg::Rax);
        let (req_ptr, abs) = if nr == SYS_CLOCK_NANOSLEEP {
            (cpu.reg(Reg::Rdx), cpu.reg(Reg::Rsi) & TIMER_ABSTIME != 0)
        } else {
            (cpu.reg(Reg::Rdi), false)
        };
        cpu.set_reg(Reg::Rax, 0);
        let mut ts = [0u8; 16];
        if vm.read_bytes(req_ptr, &mut ts).is_err() {
            return SyscallOutcome::Continue; // bad timespec → succeed without sleeping
        }
        let sec = i64::from_le_bytes(ts[0..8].try_into().unwrap());
        let nsec = i64::from_le_bytes(ts[8..16].try_into().unwrap());
        let want = (sec.max(0) as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(nsec.max(0) as u64);
        let dur_ns = if abs {
            // Absolute deadline in the reported clock domain (base + monotonic ns).
            let target_mono = want.saturating_sub((CLOCK_BASE_SEC as u64) * 1_000_000_000);
            target_mono.saturating_sub(self.now_ns())
        } else {
            want
        };
        SyscallOutcome::Sleep(Duration::from_nanos(dur_ns))
    }

    /// The mt-mode `epoll_pwait`/`epoll_wait` intercept. A zero timeout can't block, so
    /// service it inline under the shim lock (Go's `netpoll(0)` calls this constantly
    /// from `findRunnable` — no outcome churn on the hot path). A nonzero timeout yields
    /// [`SyscallOutcome::EpollWait`] carrying the epoll fd's `Arc` (kept alive across the
    /// block), so the driver runs the host wait outside the lock (go-caddy P4).
    fn epoll_wait_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        let epfd_g = cpu.reg(Reg::Rdi);
        let events_ptr = cpu.reg(Reg::Rsi);
        let maxevents = cpu.reg(Reg::Rdx) as i64;
        let timeout_ms = cpu.reg(Reg::R10) as i64;
        let epfd = match self.fs.fd_table.get(&epfd_g) {
            Some(Fd::Epoll(rc)) => rc.clone(),
            _ => {
                cpu.set_reg(Reg::Rax, EBADF);
                return SyscallOutcome::Continue;
            }
        };
        if maxevents <= 0 {
            cpu.set_reg(Reg::Rax, EINVAL);
            return SyscallOutcome::Continue;
        }
        if timeout_ms == 0 {
            // Nonblocking poll: serve inline, no yield.
            let ret = do_epoll_wait(epfd.as_raw_fd(), vm, events_ptr, maxevents as usize, 0);
            cpu.set_reg(Reg::Rax, ret);
            return SyscallOutcome::Continue;
        }
        SyscallOutcome::EpollWait {
            epfd,
            events_ptr,
            maxevents: maxevents as usize,
            timeout: (timeout_ms > 0).then(|| Duration::from_millis(timeout_ms as u64)),
        }
    }

    /// The mt-mode `read` intercept (task-125). Serve inline whenever the read can't block
    /// — a file/stdin/passthrough fd, an empty pipe with no writers (EOF), a pipe with data
    /// waiting, or a host fd that `poll`s readable (the epoll `timeout==0` fast path,
    /// applied per-fd). Only a would-block (an empty pipe with a live writer, or a host fd
    /// that isn't readable) yields [`SyscallOutcome::BlockingRead`] so the driver parks
    /// outside the shim lock. A guest fd that isn't host-backed (an unknown number)
    /// delegates so its `-EBADF` matches the single-process handler.
    fn read_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        let fd = cpu.reg(Reg::Rdi);
        let buf = cpu.reg(Reg::Rsi);
        let len = cpu.reg(Reg::Rdx) as usize;
        match self.read_would_block(fd) {
            Some(target) => SyscallOutcome::BlockingRead { target, buf, len },
            None => {
                // Data ready / EOF / a non-blocking fd: serve inline like the epoll fast
                // path. A file/stdin/unknown fd also lands here (never blocks).
                let ret = self.do_read(vm, fd, buf, len);
                cpu.set_reg(Reg::Rax, ret);
                SyscallOutcome::Continue
            }
        }
    }

    /// The mt-mode `readv` intercept (task-125). A `readv` blocks exactly when its *first*
    /// non-empty segment would block on the fd, so probe that fd: if it would block, yield a
    /// [`SyscallOutcome::BlockingRead`] targeting only the first segment (the guest reissues
    /// for the rest — a short `readv` return is POSIX-legal); otherwise scatter inline via
    /// the same loop `handle` uses.
    fn readv_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        let fd = cpu.reg(Reg::Rdi);
        let iov = cpu.reg(Reg::Rsi);
        let cnt = cpu.reg(Reg::Rdx);
        let segs = read_iovecs(vm, iov, cnt);
        // Would the read block? Probe once, against the first non-empty segment only.
        if let Some((base, seg_len)) = segs.iter().copied().find(|&(_, l)| l != 0) {
            if let Some(target) = self.read_would_block(fd) {
                return SyscallOutcome::BlockingRead {
                    target,
                    buf: base,
                    len: seg_len,
                };
            }
        }
        // Inline scatter, mirroring the single-process `SYS_READV` arm (short-read = EOF).
        // task-231: on a blocking-mode host fd, a segment after the first can `do_read` a
        // fd that segment 1 just drained empty — a `libc::read` that blocks *while holding
        // the shim lock* (same lock-held-blocking hazard as task-230). Before a subsequent
        // host-fd segment, probe `fd_readable`; if it would now block, stop and return the
        // bytes already scattered (a short `readv` is POSIX-legal — the guest reissues). The
        // first segment already passed the `read_would_block` probe above, so it may proceed;
        // pipes/files never block here, so the guard only fences host fds.
        let host_fd = self.fs.host_io_fd(fd);
        let mut total = 0u64;
        let mut first = true;
        for (base, seg_len) in segs {
            if seg_len == 0 {
                continue;
            }
            if !first {
                if let Some(h) = host_fd {
                    if !fd_readable(h) {
                        break; // would block on an emptied host fd → short read, no block
                    }
                }
            }
            first = false;
            let n = self.do_read(vm, fd, base, seg_len);
            if (n as i64) < 0 {
                if total == 0 {
                    total = n;
                }
                break;
            }
            total += n;
            if (n as usize) < seg_len {
                break;
            }
        }
        cpu.set_reg(Reg::Rax, total);
        SyscallOutcome::Continue
    }

    /// The mt-mode `accept`/`accept4` intercept (task-125). A pending connection is served
    /// inline (`accept4` returns immediately, install the fd under the lock we already
    /// hold); a listen socket with no waiting peer yields [`SyscallOutcome::BlockingAccept`]
    /// so the driver `poll`s it outside the shim lock. A non-socket fd → `-EBADF`, matching
    /// the single-process arm.
    fn accept_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        let fd = cpu.reg(Reg::Rdi);
        let addr_ptr = cpu.reg(Reg::Rsi);
        let addrlen_ptr = cpu.reg(Reg::Rdx);
        let flags = if cpu.reg(Reg::Rax) == SYS_ACCEPT4 {
            cpu.reg(Reg::R10) as libc::c_int
        } else {
            0
        };
        // Resolve the listen socket's `Arc` (kept alive across the block) and its raw fd.
        let listen = match self.fs.fd_table.get(&fd) {
            Some(Fd::Socket(rc)) => rc.clone(),
            _ => {
                cpu.set_reg(Reg::Rax, EBADF);
                return SyscallOutcome::Continue;
            }
        };
        // A connection already pending, OR a nonblocking listen fd (Go's netpoller sets
        // O_NONBLOCK and wants the immediate `-EAGAIN` `accept4` gives when no peer is
        // waiting — never a park): accept inline and install the fd under the shim lock we
        // already hold (the epoll `timeout==0` fast path). Only a *blocking-mode* listen fd
        // with no pending peer yields (task-125).
        let h = listen.as_raw_fd();
        if fd_is_nonblocking(h) || fd_readable(h) {
            let ret = self.do_accept(vm, h, addr_ptr, addrlen_ptr, flags);
            cpu.set_reg(Reg::Rax, ret);
            return SyscallOutcome::Continue;
        }
        SyscallOutcome::BlockingAccept {
            listen,
            addr_ptr,
            addrlen_ptr,
            flags,
        }
    }

    /// The mt-mode `recvfrom` intercept (task-233). Serve inline (unchanged host `recvfrom` +
    /// writeback via [`do_recvfrom`]) when the socket is nonblocking (Go's netpoller wants the
    /// immediate `-EAGAIN`, never a park), already readable (data/HUP), or not a host socket
    /// (`-EBADF`, like the single-process arm). Only a *blocking-mode* host socket with no data
    /// ready yields [`SyscallOutcome::BlockingRecv`] so the driver parks outside the shim lock.
    fn recvfrom_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        let fd = cpu.reg(Reg::Rdi);
        let buf = cpu.reg(Reg::Rsi);
        let len = cpu.reg(Reg::Rdx) as usize;
        let flags = cpu.reg(Reg::R10) as libc::c_int;
        let src = cpu.reg(Reg::R8);
        let addrlen_ptr = cpu.reg(Reg::R9);
        // Only a real host *socket* can block-yield; anything else falls to the inline path
        // (which returns -EBADF for a non-socket, matching the single-process arm).
        let sock = match self.fs.fd_table.get(&fd) {
            Some(Fd::Socket(rc)) => rc.clone(),
            _ => {
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => do_recvfrom(vm, h, buf, len, flags, src, addrlen_ptr),
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                return SyscallOutcome::Continue;
            }
        };
        let h = sock.as_raw_fd();
        // `MSG_DONTWAIT` (a per-call nonblock flag, unlike read/accept) forces this one recv
        // nonblocking regardless of the fd's O_NONBLOCK state — serve inline so an empty
        // socket returns -EAGAIN instead of parking (task-233 review).
        if fd_is_nonblocking(h) || flags & libc::MSG_DONTWAIT != 0 || fd_readable(h) {
            let ret = do_recvfrom(vm, h, buf, len, flags, src, addrlen_ptr);
            cpu.set_reg(Reg::Rax, ret);
            return SyscallOutcome::Continue;
        }
        SyscallOutcome::BlockingRecv(Box::new(BlockingRecv {
            fd: sock,
            flags,
            kind: RecvKind::Recvfrom {
                buf,
                len,
                src,
                addrlen_ptr,
            },
        }))
    }

    /// The mt-mode `recvmsg` intercept (task-233) — the `recvmsg` sibling of [`recvfrom_mt`].
    /// Same inline-vs-yield decision; the completion re-reads the iovecs/control buffer from
    /// the guest `msghdr` at `msgp`, so only that pointer is carried across the block.
    fn recvmsg_mt(&mut self, cpu: &mut Vcpu, vm: &Vm) -> SyscallOutcome {
        let fd = cpu.reg(Reg::Rdi);
        let msgp = cpu.reg(Reg::Rsi);
        let flags = cpu.reg(Reg::R10) as libc::c_int;
        let sock = match self.fs.fd_table.get(&fd) {
            Some(Fd::Socket(rc)) => rc.clone(),
            _ => {
                let ret = match self.fs.socket_fd(fd) {
                    Some(h) => do_recvmsg(vm, h, msgp, flags),
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                return SyscallOutcome::Continue;
            }
        };
        let h = sock.as_raw_fd();
        // `MSG_DONTWAIT` forces this one recv nonblocking regardless of the fd's O_NONBLOCK
        // state — serve inline so an empty socket returns -EAGAIN instead of parking
        // (task-233 review).
        if fd_is_nonblocking(h) || flags & libc::MSG_DONTWAIT != 0 || fd_readable(h) {
            let ret = do_recvmsg(vm, h, msgp, flags);
            cpu.set_reg(Reg::Rax, ret);
            return SyscallOutcome::Continue;
        }
        SyscallOutcome::BlockingRecv(Box::new(BlockingRecv {
            fd: sock,
            flags,
            kind: RecvKind::Recvmsg { msgp },
        }))
    }

    /// The driver calls this **after** the block, with the shim lock re-acquired, to finish a
    /// parked `recvfrom`/`recvmsg` from the socket `Arc` it held across the block (task-233).
    /// Runs the real recv from the raw fd — not by re-resolving the guest fd, which a sibling
    /// may have closed — reusing the exact inline writeback ([`do_recvfrom`]/[`do_recvmsg`]),
    /// so the sockaddr/iovec/control-message copies stay byte-identical to the ready-data path.
    ///
    /// Returns `Some(rax)` when the recv completed, or `None` when a sibling won the readiness
    /// race on a *shared blocking-mode* socket and the driver must re-park (task-230's class,
    /// carried into recv). The lost-race window: two threads share one blocking socket; the
    /// level-triggered `fd_readable` probe wakes both on one datagram; the first drains it; the
    /// second would then do a **blocking** `libc::recvfrom`/`recvmsg` on an empty fd *while
    /// holding the shim lock* → whole-process deadlock. We `poll(POLLIN, 0)` the fd under the
    /// lock first: still readable → the recv can't block (we hold the lock, no sibling can
    /// drain concurrently), so do it; not readable → `None`, re-park.
    pub fn recv_ready(&mut self, vm: &Vm, req: &BlockingRecv) -> Option<u64> {
        let h = req.fd.as_raw_fd();
        // Poll-under-lock (task-230 class): a sibling sharing this blocking-mode socket may have
        // drained it after the driver's level-triggered probe woke us both. If it's no longer
        // readable, a `libc::recv*` here would block on an empty fd *while holding the shim
        // lock* → whole-process deadlock. Re-park instead.
        if !fd_readable(h) {
            return None;
        }
        Some(match req.kind {
            RecvKind::Recvfrom {
                buf,
                len,
                src,
                addrlen_ptr,
            } => do_recvfrom(vm, h, buf, len, req.flags, src, addrlen_ptr),
            RecvKind::Recvmsg { msgp } => do_recvmsg(vm, h, msgp, req.flags),
        })
    }

    /// Would a threaded `read(fd)` block, and on what? `Some(target)` means the driver must
    /// park (an empty pipe with a live writer, or a host socket/eventfd that `poll`s
    /// not-readable); `None` means serve inline — data is ready, it's EOF, or the fd never
    /// blocks (file/stdin/unknown). Mirrors [`pipe_would_block`](Self::pipe_would_block) for
    /// the in-process case and the epoll `timeout==0` probe for the host-fd case.
    fn read_would_block(&self, fd: u64) -> Option<ReadTarget> {
        match self.fs.fd_table.get(&fd) {
            Some(Fd::PipeRead(rc)) => {
                let b = rc.lock().unwrap();
                // Empty with a live writer → would block; data present or no writer (EOF)
                // → serve inline. A nonblocking read end (task-232) never yields: it's
                // served inline where `do_read` returns the `-EAGAIN` the guest polls for.
                (b.data.is_empty() && b.writers > 0 && !b.nonblocking)
                    .then(|| ReadTarget::Pipe(rc.clone()))
            }
            Some(Fd::Socket(rc) | Fd::Event(rc)) => {
                let h = rc.as_raw_fd();
                // A guest that put the fd in O_NONBLOCK (Go's netpoller sockets, its
                // netpollBreak eventfd drain) *wants* an immediate `-EAGAIN`, never a park:
                // that errno is how it drives its epoll scheduler. Only a *blocking-mode*
                // fd that isn't readable yields; a nonblocking one is served inline, where
                // the real `read` returns the EAGAIN the guest is polling for (task-125).
                let block_yield = !fd_is_nonblocking(h) && !fd_readable(h);
                block_yield.then(|| ReadTarget::Host(rc.clone()))
            }
            _ => None, // file / stdin / epoll / unknown: never a yielding blocking read
        }
    }

    /// Real host `accept4` on listen fd `h`, writing the peer `sockaddr` back to the guest
    /// and installing the accepted socket in `fd_table` under the shim lock. Returns the
    /// new guest fd or a negative errno. Shared by the inline (`accept_mt`) and post-block
    /// (driver, via [`accept_ready`](Self::accept_ready)) paths so the fd-table mutation
    /// lives in exactly one place, always under the lock.
    fn do_accept(
        &mut self,
        vm: &Vm,
        h: i32,
        addr_ptr: u64,
        addrlen_ptr: u64,
        flags: libc::c_int,
    ) -> u64 {
        let mut sa = [0u8; 128];
        let mut sl = sa.len() as libc::socklen_t;
        let want_addr = addr_ptr != 0;
        let (aptr, alptr) = if want_addr {
            (
                sa.as_mut_ptr() as *mut libc::sockaddr,
                &mut sl as *mut libc::socklen_t,
            )
        } else {
            (std::ptr::null_mut(), std::ptr::null_mut())
        };
        let r = unsafe { libc::accept4(h, aptr, alptr, flags) };
        if r < 0 {
            return host_errno();
        }
        write_sockaddr(vm, addr_ptr, addrlen_ptr, &sa, sl);
        let owned = unsafe { OwnedFd::from_raw_fd(r) };
        let g = self.fs.alloc_fd();
        self.fs.fd_table.insert(g, Fd::Socket(Arc::new(owned)));
        g
    }

    /// The driver calls this **after** the block, with the shim lock re-acquired, to finish
    /// a parked `accept`: the listen fd is now readable, so do the real `accept4` and
    /// install the accepted socket (fd allocation is shim state, so it must run under the
    /// lock — task-125).
    ///
    /// Returns `Some(rax)` (new fd or a negative errno) when the accept completed, or `None`
    /// when a sibling won the connection-readiness race and the driver must re-park
    /// (task-230). The lost-race window is the accept analogue of [`read_ready`](Self::read_ready):
    /// two threads share (or dup) one blocking-mode listen fd; the level-triggered
    /// `fd_readable` probe wakes both on one pending connection; the first accepts it; the
    /// second would then do a **blocking** `libc::accept4` on a peer-less listen fd *while
    /// holding the shim lock* → whole-process deadlock. We `poll(POLLIN, 0)` the listen fd
    /// under the lock first: still readable → the accept can't block (we hold the lock, no
    /// sibling can drain the backlog concurrently), so do it; not readable → `None`, re-park.
    ///
    /// This guard lives here, on the *post-block* path only. The inline [`accept_mt`](Self::accept_mt)
    /// path already runs its `fd_readable` probe and `do_accept` atomically under the shim
    /// lock in `handle_mt` (no lost-race window), and the single-threaded [`handle`](Self::handle)
    /// accept is unaffected — both call `do_accept` directly, unchanged.
    pub fn accept_ready(
        &mut self,
        vm: &Vm,
        listen: i32,
        addr_ptr: u64,
        addrlen_ptr: u64,
        flags: libc::c_int,
    ) -> Option<u64> {
        // Poll-under-lock (task-230): a sibling sharing this listen fd may have accepted the
        // pending connection after the driver's probe woke us both. If no connection is now
        // pending, a `libc::accept4` here would block on a peer-less listen fd *while holding
        // the shim lock* → whole-process deadlock. Re-park instead.
        if !fd_readable(listen) {
            return None;
        }
        Some(self.do_accept(vm, listen, addr_ptr, addrlen_ptr, flags))
    }

    /// The driver calls this **after** the block, with the shim lock re-acquired, to finish
    /// a parked `read` from the [`ReadTarget`] it held across the block (task-125). Reads
    /// straight from the target — not by re-resolving the guest fd, which a sibling may have
    /// closed — into guest `buf`. A pipe drains up to `len` bytes (empty → EOF `0`); a host
    /// fd does one real `read`. Uses the shim scratch buffer, so it must run under the shim
    /// lock.
    ///
    /// Returns `Some(rax)` (byte count or a negative errno) when the read completed, or
    /// `None` when a sibling won the readiness race on a *shared blocking-mode* host fd and
    /// the driver must re-park (task-230). The lost-race window: two threads share one
    /// blocking host fd; the level-triggered `read_target_ready` probe wakes both on one
    /// event; the first drains the fd; the second would then do a **blocking** `libc::read`
    /// on an empty fd *while holding the shim lock* — deadlocking every sibling. We
    /// `poll(POLLIN, 0)` the fd under the lock first: still readable → the read can't block
    /// (we hold the lock, no sibling can drain concurrently), so do it; not readable →
    /// `None`, re-park. The pipe target drains an `Arc<Mutex<PipeBuf>>` and never issues a
    /// blocking host syscall, so it always returns `Some`.
    pub fn read_ready(
        &mut self,
        vm: &Vm,
        target: &ReadTarget,
        buf: u64,
        len: usize,
    ) -> Option<u64> {
        match target {
            ReadTarget::Pipe(rc) => {
                let chunk: Vec<u8> = {
                    let mut b = rc.lock().unwrap();
                    let n = len.min(b.data.len());
                    b.data.drain(..n).collect()
                };
                if vm.write_bytes(buf, &chunk).is_err() {
                    return Some(EFAULT);
                }
                Some(chunk.len() as u64)
            }
            ReadTarget::Host(rc) => {
                let h = rc.as_raw_fd();
                // Poll-under-lock (task-230): a sibling sharing this blocking-mode fd may
                // have drained it after the driver's level-triggered probe woke us both. If
                // it's no longer readable, a `libc::read` here would block on an empty fd
                // *while holding the shim lock* → whole-process deadlock. Re-park instead.
                if !fd_readable(h) {
                    return None;
                }
                if !self.try_resize_scratch(len) {
                    return Some(EFAULT);
                }
                let n =
                    unsafe { libc::read(h, self.scratch.as_mut_ptr() as *mut libc::c_void, len) };
                if n < 0 {
                    return Some(host_errno());
                }
                let n = n as usize;
                if vm.write_bytes(buf, &self.scratch[..n]).is_err() {
                    return Some(EFAULT);
                }
                Some(n as u64)
            }
        }
    }

    /// Resolve a guest `open`: read the C-string path from guest memory, check it
    /// against the allowlist, and host-open read-only. Returns a guest fd or a
    /// negative errno.
    fn do_open(&mut self, vm: &Vm, path_ptr: u64, flags: u64) -> u64 {
        let path = read_cstr(vm, path_ptr);
        if (flags & O_ACCMODE) != O_RDONLY {
            // Writable open: only under a permitted write dir, mapped to a real file.
            let Some(host) = self.fs.resolve_host_write(&path) else {
                return EACCES;
            };
            let mut opts = std::fs::OpenOptions::new();
            opts.read(true).write(true);
            opts.create(flags & O_CREAT != 0);
            opts.truncate(flags & O_TRUNC != 0);
            if flags & O_EXCL != 0 {
                opts.create_new(true);
            }
            return match opts.open(&host) {
                Ok(f) => {
                    let fd = self.fs.alloc_fd();
                    self.fs
                        .fd_table
                        .insert(fd, Fd::File(Arc::new(Mutex::new(OpenEntry::File(f)))));
                    fd
                }
                Err(_) => ENOENT,
            };
        }
        // Not resolvable → "no such file" (a dynamic loader probes many paths).
        let Some(host) = self.fs.resolve_host(&path) else {
            return ENOENT;
        };
        let Ok(meta) = std::fs::metadata(&host) else {
            return ENOENT;
        };
        let entry = if meta.is_dir() {
            let mut entries = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&host) {
                for e in rd.flatten() {
                    let ft = e.file_type().ok();
                    let dtype = match ft {
                        Some(t) if t.is_dir() => 4,      // DT_DIR
                        Some(t) if t.is_symlink() => 10, // DT_LNK
                        _ => 8,                          // DT_REG
                    };
                    entries.push(DirEnt {
                        name: e.file_name().as_encoded_bytes().to_vec(),
                        ino: e.metadata().map(|m| m.ino()).unwrap_or(1),
                        dtype,
                    });
                }
            }
            OpenEntry::Dir(Box::new(DirState {
                meta,
                entries,
                pos: 0,
            }))
        } else {
            match File::open(&host) {
                Ok(f) => OpenEntry::File(f),
                Err(_) => return ENOENT,
            }
        };
        let fd = self.fs.alloc_fd();
        self.fs
            .fd_table
            .insert(fd, Fd::File(Arc::new(Mutex::new(entry))));
        fd
    }

    /// Resolve a guest `read`: pull bytes from the host file into a scratch buffer,
    /// then copy them into guest memory. Returns the byte count or a negative errno.
    /// `write(fd, buf, len)`: route the `len` bytes at guest `buf` to the fd's sink
    /// (captured stdout/stderr, a passthrough file, a pipe, or a host socket). Shared
    /// by the x86-64 `SYS_WRITE` arm and the i386 `int 0x80` path — the byte plumbing
    /// is ABI-independent; only the register/number decode differs.
    fn do_write(&mut self, vm: &Vm, fd: u64, buf: u64, len: usize) -> u64 {
        if !self.try_fill_scratch(vm, buf, len) {
            return EFAULT; // unmapped/bogus source → -EFAULT, no panic
        }
        match self.fs.fd_table.get(&fd) {
            Some(Fd::Stdout) => {
                self.stdout.extend_from_slice(&self.scratch);
                len as u64
            }
            Some(Fd::Stderr) => {
                self.stderr.extend_from_slice(&self.scratch);
                len as u64
            }
            // A writable passthrough file: append at the current position.
            Some(Fd::File(rc)) => match rc.lock().unwrap().as_file_mut() {
                Some(f) => match f.write(&self.scratch) {
                    Ok(n) => n as u64,
                    Err(_) => EBADF,
                },
                None => len as u64,
            },
            Some(Fd::PipeWrite(rc)) => {
                rc.lock().unwrap().data.extend(self.scratch.iter().copied());
                len as u64
            }
            // A real host socket or eventfd: forward the bytes to the host fd
            // (Go's netpollBreak writes 8 bytes to the eventfd).
            Some(Fd::Socket(rc) | Fd::Event(rc)) => {
                let h = rc.as_raw_fd();
                let n =
                    unsafe { libc::write(h, self.scratch.as_ptr() as *const libc::c_void, len) };
                if n < 0 {
                    host_errno()
                } else {
                    n as u64
                }
            }
            Some(Fd::PipeRead(_)) => EBADF, // write to the read end
            Some(Fd::Epoll(_)) => EBADF,    // an epoll fd isn't writable
            // stdin or an unknown fd: swallow (matches prior behavior).
            Some(Fd::Stdin) | None => len as u64,
        }
    }

    fn do_read(&mut self, vm: &Vm, fd: u64, buf: u64, len: usize) -> u64 {
        // A passthrough file takes precedence — a tool can `dup2` its input onto
        // fd 0 and then read "stdin" (busybox gunzip does exactly this).
        if let Some(rc) = self.fs.file(fd) {
            let mut entry = rc.lock().unwrap();
            let Some(file) = entry.as_file_mut() else {
                return EBADF;
            };
            // `rc` is an owned clone, so `entry` borrows the RefCell, not `self` — the
            // scratch resize can run while the file is borrowed.
            if !self.try_resize_scratch(len) {
                return EFAULT;
            }
            return match file.read(&mut self.scratch) {
                Ok(n) => {
                    if vm.write_bytes(buf, &self.scratch[..n]).is_err() {
                        return EFAULT; // bad guest buffer → -EFAULT, never panic the host
                    }
                    n as u64
                }
                Err(_) => EBADF,
            };
        }
        if let Some(h) = self.fs.host_io_fd(fd) {
            // Host read from a socket or eventfd. A nonblocking fd (Go's netpoller fds,
            // and its eventfd drain) returns instantly or -EAGAIN; a blocking socket
            // read still blocks the calling thread inline (fine for Go, which is always
            // nonblocking — a blocking mt read is task-125). Go drains its netpollBreak
            // eventfd with an 8-byte read here.
            if !self.try_resize_scratch(len) {
                return EFAULT;
            }
            let n = unsafe { libc::read(h, self.scratch.as_mut_ptr() as *mut libc::c_void, len) };
            return if n < 0 {
                host_errno()
            } else {
                let n = n as usize;
                if vm.write_bytes(buf, &self.scratch[..n]).is_err() {
                    return EFAULT; // peer bytes already consumed, but never panic the host
                }
                n as u64
            };
        }
        if let Some(rc) = self.fs.pipe_read(fd) {
            // Drain up to `len` bytes; an empty buffer reads as EOF (0). The deferred
            // model runs the writer to completion first, so the data is already here.
            // task-232: a *nonblocking* read end with an empty buffer and a live writer
            // returns `-EAGAIN` (not EOF) — the self-pipe / event-loop contract.
            let chunk: Vec<u8> = {
                let mut b = rc.lock().unwrap();
                if b.data.is_empty() && b.writers > 0 && b.nonblocking {
                    return EAGAIN;
                }
                let n = len.min(b.data.len());
                b.data.drain(..n).collect()
            };
            if vm.write_bytes(buf, &chunk).is_err() {
                return EFAULT;
            }
            return chunk.len() as u64;
        }
        if fd == 0 {
            // Real stdin: drain the scripted buffer, EOF (0) once exhausted.
            let n = len.min(self.stdin.len() - self.stdin_pos);
            let chunk = self.stdin[self.stdin_pos..self.stdin_pos + n].to_vec();
            if vm.write_bytes(buf, &chunk).is_err() {
                return EFAULT;
            }
            self.stdin_pos += n;
            return n as u64;
        }
        EBADF
    }
}

/// Does `haystack` contain `needle` as a contiguous subslice?
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Read a little-endian `u64` from guest memory (0 if unmapped).
fn read_u64(vm: &Vm, addr: u64) -> u64 {
    let mut b = [0u8; 8];
    if vm.read_bytes(addr, &mut b).is_ok() {
        u64::from_le_bytes(b)
    } else {
        0
    }
}

/// Walk a guest `struct iovec[cnt]` at `iov` (each entry is `{ void* base; size_t len; }`,
/// 16 bytes: base @0, len @8) and return the `(base, len)` pairs. A bad guest pointer
/// reads back as 0 (via `read_u64`), matching every call site's existing behavior, so the
/// short-read/EFAULT handling stays where it already is (per-segment I/O still decides).
fn read_iovecs(vm: &Vm, iov: u64, cnt: u64) -> Vec<(u64, usize)> {
    (0..cnt)
        .map(|i| {
            let base = read_u64(vm, iov.wrapping_add(i * 16));
            let len = read_u64(vm, iov.wrapping_add(i * 16).wrapping_add(8)) as usize;
            (base, len)
        })
        .collect()
}

/// One host `epoll_wait` (retrying `EINTR`), marshaling up to `maxevents` (capped at
/// 1024) ready events into the guest array. The guest `epoll_event` is 12 bytes packed
/// (u32 events @0, u64 data @4); write each field at its explicit offset so it's correct
/// on a 16-byte-aligned aarch64 host too. Returns the guest `Rax`: event count, or
/// `-errno` (go-caddy P4).
pub(crate) fn do_epoll_wait(
    epfd_h: i32,
    vm: &Vm,
    events_ptr: u64,
    maxevents: usize,
    timeout_ms: i32,
) -> u64 {
    let cap = maxevents.min(1024);
    let mut buf: Vec<libc::epoll_event> = vec![unsafe { std::mem::zeroed() }; cap];
    let n = loop {
        let n =
            unsafe { libc::epoll_wait(epfd_h, buf.as_mut_ptr(), cap as libc::c_int, timeout_ms) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue; // a signal (none delivered here) — retry
            }
            return (-(e.raw_os_error().unwrap_or(libc::EINVAL) as i64)) as u64;
        }
        break n as usize;
    };
    for (i, slot) in buf.iter().take(n).enumerate() {
        let events = slot.events; // copy out of the packed struct before use
        let data = slot.u64;
        let base = events_ptr.wrapping_add((i as u64) * 12); // guest epoll_event stride = 12
        let _ = vm.write_bytes(base, &events.to_le_bytes());
        let _ = vm.write_bytes(base.wrapping_add(4), &data.to_le_bytes());
    }
    n as u64
}

/// `sigaltstack(new, old)` against a caller-owned [`SigAltStack`] slot (a `ThreadCtx`
/// field when threaded, the shim field otherwise). Writes the current stack to `old`
/// (so a query reads back `SS_DISABLE`, not garbage), then installs `new`. Returns the
/// guest `Rax`: 0 / -EFAULT (unreadable `new`) / -ENOMEM (stack too small). No delivery.
fn do_sigaltstack(cpu: &mut Vcpu, vm: &Vm, cur: &mut SigAltStack) -> u64 {
    // stack_t { void *ss_sp; int ss_flags; size_t ss_size } — 24 bytes, flags at +8.
    const MINSIGSTKSZ: u64 = 2048;
    let new = cpu.reg(Reg::Rdi);
    let old = cpu.reg(Reg::Rsi);
    if old != 0 {
        let mut buf = [0u8; 24];
        buf[0..8].copy_from_slice(&cur.sp.to_le_bytes());
        buf[8..12].copy_from_slice(&cur.flags.to_le_bytes());
        buf[16..24].copy_from_slice(&cur.size.to_le_bytes());
        let _ = vm.write_bytes(old, &buf);
    }
    if new != 0 {
        let mut buf = [0u8; 24];
        if vm.read_bytes(new, &mut buf).is_err() {
            return EFAULT;
        }
        let flags = i32::from_le_bytes(buf[8..12].try_into().unwrap());
        if flags & SS_DISABLE != 0 {
            *cur = SigAltStack::default();
        } else {
            let size = u64::from_le_bytes(buf[16..24].try_into().unwrap());
            if size < MINSIGSTKSZ {
                return ENOMEM;
            }
            *cur = SigAltStack {
                sp: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
                size,
                flags,
            };
        }
    }
    0
}

/// `rt_sigprocmask(how, set, oldset)` against a caller-owned mask (a `ThreadCtx` field
/// when threaded, the shim field otherwise). Writes the old mask to `oldset`, then
/// applies `set` per `how`. No delivery — the mask is bookkeeping Go reads back.
fn do_sigprocmask(cpu: &mut Vcpu, vm: &Vm, mask: &mut u64) -> u64 {
    const SIG_BLOCK: u64 = 0;
    const SIG_UNBLOCK: u64 = 1;
    const SIG_SETMASK: u64 = 2;
    let how = cpu.reg(Reg::Rdi);
    let set = cpu.reg(Reg::Rsi);
    let old = cpu.reg(Reg::Rdx);
    if old != 0 {
        let _ = vm.write_bytes(old, &mask.to_le_bytes());
    }
    if set != 0 {
        let mut b = [0u8; 8];
        if vm.read_bytes(set, &mut b).is_ok() {
            let nv = u64::from_le_bytes(b);
            match how {
                SIG_BLOCK => *mask |= nv,
                SIG_UNBLOCK => *mask &= !nv,
                SIG_SETMASK => *mask = nv,
                _ => return EINVAL,
            }
        }
    }
    0
}

/// Read a little-endian `u32` from guest memory (0 if unmapped) — the in/out
/// `addrlen` and `optlen` words the socket calls pass.
fn read_u32(vm: &Vm, addr: u64) -> u32 {
    let mut b = [0u8; 4];
    if vm.read_bytes(addr, &mut b).is_ok() {
        u32::from_le_bytes(b)
    } else {
        0
    }
}

/// Is host fd `h` readable *right now*? A non-blocking `poll(POLLIN, timeout=0)` — the
/// per-fd analogue of the epoll `timeout==0` fast path (task-125). `true` when data (or a
/// pending connection, for a listen socket) is waiting or the peer hung up (`POLLHUP`), so a
/// following `read`/`accept` completes without blocking; `false` (including on a `poll`
/// error, which a subsequent real op reports honestly) means the driver should park.
pub(crate) fn fd_readable(h: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd: h,
        events: libc::POLLIN,
        revents: 0,
    };
    let n = unsafe { libc::poll(&mut pfd, 1, 0) };
    // POLLIN → data/connection ready; POLLHUP/POLLERR → the next read returns 0/-errno
    // immediately (also "won't block"). Treat any revents as ready-to-serve-inline.
    n > 0 && pfd.revents != 0
}

/// Is host fd `h` in `O_NONBLOCK` mode? A guest that set it that way (Go's netpoller
/// sockets and its eventfd) expects `read`/`accept` to return `-EAGAIN` immediately, never
/// to block — so the mt intercept must serve it inline, not park (task-125). A `fcntl`
/// error conservatively reports blocking (the safe default: probe readiness first).
fn fd_is_nonblocking(h: i32) -> bool {
    let flags = unsafe { libc::fcntl(h, libc::F_GETFL) };
    flags >= 0 && (flags & libc::O_NONBLOCK) != 0
}

/// `-errno` in RAX from the host's last failed syscall, the way the kernel returns
/// it to a guest (a small negative). Falls back to `-EINVAL` if the host didn't set
/// one.
fn host_errno() -> u64 {
    let e = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EINVAL);
    (-(e as i64)) as u64
}

/// Copy a host-filled `sockaddr` back into the guest's `addr`/`addrlen` out-params
/// (accept/getsockname/getpeername). `addrlen` is in/out: the guest word gives the
/// buffer size, and the actual length `sl` is written back even if it was truncated
/// (POSIX). No-op if the guest passed NULL for either pointer.
fn write_sockaddr(vm: &Vm, addr: u64, addrlen_ptr: u64, sa: &[u8], sl: libc::socklen_t) {
    if addr == 0 || addrlen_ptr == 0 {
        return;
    }
    let bufsize = read_u32(vm, addrlen_ptr) as usize;
    let n = (sl as usize).min(bufsize).min(sa.len());
    let _ = vm.write_bytes(addr, &sa[..n]);
    let _ = vm.write_bytes(addrlen_ptr, &sl.to_le_bytes());
}

/// The real host `recvfrom` + guest writeback, factored out so the inline `handle` arm and
/// the post-block completion ([`LinuxShim::recv_ready`], task-233) share byte-identical
/// sockaddr writeback and MSG_TRUNC clamping. Receives into a host buffer, copies back to the
/// guest, and (if `src != 0`) writes the peer address. Returns the RAX value: the true packet
/// length (which MSG_TRUNC may report larger than the buffer) or a negative errno. `h` must be
/// a readable-or-nonblocking host socket fd (the caller — inline or `recv_ready` — has already
/// gated blocking here, so this never blocks under the shim lock).
fn do_recvfrom(
    vm: &Vm,
    h: i32,
    buf: u64,
    len: usize,
    flags: libc::c_int,
    src: u64,
    addrlen_ptr: u64,
) -> u64 {
    let mut data = vec![0u8; len];
    let mut sa = [0u8; 128];
    let mut sl = sa.len() as libc::socklen_t;
    let want_addr = src != 0;
    let (aptr, alptr) = if want_addr {
        (
            sa.as_mut_ptr() as *mut libc::sockaddr,
            &mut sl as *mut libc::socklen_t,
        )
    } else {
        (std::ptr::null_mut(), std::ptr::null_mut())
    };
    let n = unsafe {
        libc::recvfrom(
            h,
            data.as_mut_ptr() as *mut libc::c_void,
            len,
            flags,
            aptr,
            alptr,
        )
    };
    if n < 0 {
        host_errno()
    } else {
        // MSG_TRUNC (and datagram reads generally) let the kernel return the *real* packet
        // length `n`, which can exceed the `len`-sized buffer we allocated — `&data[..n]`
        // would then slice out of bounds and abort the emulator. Clamp the copy to what
        // actually landed in the buffer, but still return the true `n` in RAX: reporting the
        // untruncated length is the recvfrom/MSG_TRUNC contract the guest relies on.
        let n = n as usize;
        let copy = n.min(len);
        if copy > 0 && vm.write_bytes(buf, &data[..copy]).is_err() {
            EFAULT
        } else {
            if want_addr {
                write_sockaddr(vm, src, addrlen_ptr, &sa, sl);
            }
            n as u64
        }
    }
}

/// The real host `recvmsg` + guest scatter/control writeback, factored out so the inline
/// `handle` arm and the post-block completion ([`LinuxShim::recv_ready`], task-233) share
/// byte-identical iovec scatter, control-message copy, and `msg_controllen`/`msg_flags`
/// writeback. Receives into one coalesced host buffer, scatters across the guest iovecs, and
/// copies the control (cmsg) buffer + updated flags back. Returns the RAX value (bytes
/// received or a negative errno). `h` must be a readable-or-nonblocking host socket fd (the
/// caller has already gated blocking, so this never blocks under the shim lock).
fn do_recvmsg(vm: &Vm, h: i32, msgp: u64, flags: libc::c_int) -> u64 {
    let iov = read_u64(vm, msgp.wrapping_add(16));
    let iovlen = read_u64(vm, msgp.wrapping_add(24));
    let control = read_u64(vm, msgp.wrapping_add(32));
    let controllen = read_u64(vm, msgp.wrapping_add(40)) as usize;
    let iovecs = read_iovecs(vm, iov, iovlen);
    let total: usize = iovecs.iter().map(|&(_, len)| len).sum();
    let mut data = vec![0u8; total];
    let mut cbuf = vec![0u8; controllen];
    let mut iovh = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut libc::c_void,
        iov_len: total,
    };
    let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
    mh.msg_iov = &mut iovh;
    mh.msg_iovlen = 1;
    if controllen > 0 {
        mh.msg_control = cbuf.as_mut_ptr() as *mut libc::c_void;
        mh.msg_controllen = controllen;
    }
    let n = unsafe { libc::recvmsg(h, &mut mh, flags) };
    if n < 0 {
        host_errno()
    } else {
        // Scatter the received bytes back across the guest iovecs.
        let mut off = 0usize;
        let got = n as usize;
        for &(base, len) in &iovecs {
            if off >= got {
                break;
            }
            let take = len.min(got - off);
            if take > 0 {
                let _ = vm.write_bytes(base, &data[off..off + take]);
                off += take;
            }
        }
        // Copy the returned control buffer + updated controllen + flags.
        if controllen > 0 && mh.msg_controllen > 0 {
            let clen = (mh.msg_controllen as usize).min(controllen);
            let _ = vm.write_bytes(control, &cbuf[..clen]);
        }
        let _ = vm.write_bytes(
            msgp.wrapping_add(40),
            &(mh.msg_controllen as u64).to_le_bytes(),
        );
        let _ = vm.write_bytes(msgp.wrapping_add(48), &(mh.msg_flags as i32).to_le_bytes());
        n as u64
    }
}

/// Write a minimal x86-64 `struct stat` (144 bytes) describing `meta` as a regular
/// file: enough for the size/mode checks a hashing utility makes. `st_dev`/`st_ino`
/// carry the real host values — glibc's ld.so dedupes loaded objects by that pair,
/// so a fabricated (0, 0) would collide with the main map and make it treat
/// `libc.so.6` as already loaded.
fn write_stat(vm: &Vm, addr: u64, meta: &std::fs::Metadata) {
    let size = meta.len();
    // Real mode — type bits (S_IFDIR vs S_IFREG …) so an interpreter walking its
    // stdlib distinguishes dirs from files, AND the real permission bits, since a
    // shell's PATH search rejects a hit without the execute bit (busybox `cat` in a
    // pipeline stats /bin/cat and skips it if it looks non-executable).
    let mode = meta.mode();
    let mut buf = [0u8; 144];
    buf[0..8].copy_from_slice(&meta.dev().to_le_bytes()); // st_dev
    buf[8..16].copy_from_slice(&meta.ino().to_le_bytes()); // st_ino
    buf[16..24].copy_from_slice(&1u64.to_le_bytes()); // st_nlink = 1
    buf[24..28].copy_from_slice(&mode.to_le_bytes()); // st_mode
    buf[48..56].copy_from_slice(&size.to_le_bytes()); // st_size
    buf[56..64].copy_from_slice(&512u64.to_le_bytes()); // st_blksize
    buf[64..72].copy_from_slice(&size.div_ceil(512).to_le_bytes()); // st_blocks
    let _ = vm.write_bytes(addr, &buf);
}

/// Write a synthetic `struct statfs` (x86-64, 120 bytes, all 8-byte fields) — a
/// plausible ext4-like filesystem with free space, so a guest that sizes buffers
/// or checks capacity from it proceeds instead of failing.
fn write_statfs(vm: &Vm, addr: u64) {
    let mut buf = [0u8; 120];
    buf[0..8].copy_from_slice(&0xEF53u64.to_le_bytes()); // f_type = EXT4_SUPER_MAGIC
    buf[8..16].copy_from_slice(&4096u64.to_le_bytes()); // f_bsize
    buf[16..24].copy_from_slice(&(1u64 << 20).to_le_bytes()); // f_blocks
    buf[24..32].copy_from_slice(&(1u64 << 19).to_le_bytes()); // f_bfree
    buf[32..40].copy_from_slice(&(1u64 << 19).to_le_bytes()); // f_bavail
    buf[40..48].copy_from_slice(&(1u64 << 16).to_le_bytes()); // f_files
    buf[48..56].copy_from_slice(&(1u64 << 15).to_le_bytes()); // f_ffree
                                                              // f_fsid (56..64) left zero
    buf[64..72].copy_from_slice(&255u64.to_le_bytes()); // f_namelen
    buf[72..80].copy_from_slice(&4096u64.to_le_bytes()); // f_frsize
                                                         // f_flags (80..88) + f_spare (88..120) left zero
    let _ = vm.write_bytes(addr, &buf);
}

/// Write a `struct stat` describing a character device (for stdin/stdout/stderr).
fn write_chr_stat(vm: &Vm, addr: u64) {
    let mut buf = [0u8; 144];
    buf[16..24].copy_from_slice(&1u64.to_le_bytes()); // st_nlink = 1
    buf[24..28].copy_from_slice(&0o020620u32.to_le_bytes()); // st_mode = S_IFCHR|0620
    buf[56..64].copy_from_slice(&1024u64.to_le_bytes()); // st_blksize
    let _ = vm.write_bytes(addr, &buf);
}

/// Read a NULL-terminated array of C-string pointers (argv/envp) from guest memory
/// into owned strings. Caps at 1024 entries to bound a bad pointer.
fn read_cstr_array(vm: &Vm, mut addr: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for _ in 0..1024 {
        let ptr = read_u64(vm, addr);
        if ptr == 0 {
            break;
        }
        out.push(read_cstr(vm, ptr));
        addr += 8;
    }
    out
}

/// Read a NUL-terminated string from guest memory, one byte at a time (the length
/// is unknown up front). Caps at 4096 to bound a runaway/unmapped pointer.
fn read_cstr(vm: &Vm, mut addr: u64) -> Vec<u8> {
    const CAP: usize = 4096;
    let mut out = Vec::new();
    let mut chunk = [0u8; 64];
    // Read in chunks (each `read_bytes` re-runs the region scan) instead of one byte
    // at a time; shrink the chunk at a region edge so we never read past the mapping.
    while out.len() < CAP {
        let mut n = chunk.len().min(CAP - out.len());
        while n > 0 && vm.read_bytes(addr, &mut chunk[..n]).is_err() {
            n /= 2;
        }
        if n == 0 {
            break;
        }
        if let Some(nul) = chunk[..n].iter().position(|&b| b == 0) {
            out.extend_from_slice(&chunk[..nul]);
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        addr += n as u64;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        fd_is_nonblocking, resolve_in_rootfs, BlockingRecv, EntropyMode, Fd, LinuxShim, MtClock,
        PipeBuf, ReadTarget, RecvKind, SyscallOutcome, ThreadCtx, CLOCK_TICK_NS, EFAULT, ENOENT,
        MT_CLOCK_TICK_NS, ROBUST_LIST_HEAD_SIZE, SYS_EXECVE, SYS_FCNTL, SYS_FORK, SYS_FUTEX,
        SYS_GET_ROBUST_LIST, SYS_MADVISE, SYS_MMAP, SYS_MUNMAP, SYS_PIPE2, SYS_READ,
        SYS_READLINKAT, SYS_READV, SYS_RECVFROM, SYS_SCHED_GETAFFINITY, SYS_SET_ROBUST_LIST,
        SYS_UNAME,
    };
    use crate::hostmem;
    use std::collections::VecDeque;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use x86jit_core::{InterpreterBackend, Prot, Reg, RegionKind, Vcpu, Vm, VmConfig};

    /// task-128 AC#1: Deterministic entropy reproduces byte-identical streams across
    /// runs (fresh shims), is not the old constant `0x42` fill, and advances within a
    /// run; HostEntropy differs between runs.
    #[test]
    fn getrandom_entropy_modes() {
        let stream = |mode: EntropyMode, len: usize| {
            let mut s = LinuxShim::new();
            s.set_entropy(mode);
            assert!(s.fill_scratch_entropy(len));
            s.scratch.clone()
        };

        // Deterministic: two independent runs match byte-for-byte.
        let a = stream(EntropyMode::Deterministic, 64);
        let b = stream(EntropyMode::Deterministic, 64);
        assert_eq!(a, b, "deterministic stream must reproduce across runs");
        assert!(
            a.iter().any(|&x| x != 0x42),
            "must not be the old 0x42 fill"
        );
        assert!(
            a.windows(2).any(|w| w[0] != w[1]),
            "must vary within the stream"
        );

        // Within one run, successive draws advance (not repeated).
        let mut s = LinuxShim::new();
        assert!(s.fill_scratch_entropy(32));
        let first = s.scratch.clone();
        assert!(s.fill_scratch_entropy(32));
        assert_ne!(first, s.scratch, "successive draws must advance the PRNG");

        // HostEntropy: two runs differ (real randomness). 64 bytes → collision ~2^-512.
        let h1 = stream(EntropyMode::HostEntropy, 64);
        let h2 = stream(EntropyMode::HostEntropy, 64);
        assert_ne!(h1, h2, "host entropy must differ between runs");
    }

    /// VCLK-1: `tick` returns `old + quantum` and consecutive reads strictly increase;
    /// `seed`/`peek` set and read without advancing.
    #[test]
    fn mt_clock_tick_and_seed() {
        let c = MtClock::default();
        assert_eq!(c.peek(), 0);
        assert_eq!(c.tick(MT_CLOCK_TICK_NS), MT_CLOCK_TICK_NS);
        assert_eq!(c.tick(MT_CLOCK_TICK_NS), 2 * MT_CLOCK_TICK_NS);
        assert_eq!(c.peek(), 2 * MT_CLOCK_TICK_NS, "peek doesn't advance");
        c.seed(1_000_000);
        assert_eq!(c.peek(), 1_000_000, "seed sets the value");
        assert_eq!(c.tick(5), 1_000_005);
    }

    /// VCLK-1: `advance_to` is a monotone max — a credit below the current value is a
    /// no-op, so concurrent sleepers overlap (their max) instead of summing.
    #[test]
    fn mt_clock_advance_to_is_monotone_max() {
        let c = MtClock::default();
        c.tick(100);
        c.advance_to(50); // below current → no-op
        assert_eq!(c.peek(), 100);
        c.advance_to(300); // above → raise
        assert_eq!(c.peek(), 300);
        c.advance_to(300); // equal → no-op
        assert_eq!(c.peek(), 300);
    }

    /// VCLK-1: under concurrent `tick` + `advance_to` from many threads, no sample ever
    /// decreases and the final value is at least every thread's total contribution —
    /// the atomic's single modification order is the monotonicity guarantee (M5).
    #[test]
    fn mt_clock_concurrent_never_decreases() {
        const THREADS: u64 = 8;
        const ITERS: u64 = 10_000;
        let c = Arc::new(MtClock::default());
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let c = Arc::clone(&c);
                std::thread::spawn(move || {
                    let mut last = 0;
                    for _ in 0..ITERS {
                        let now = c.tick(2);
                        assert!(now > last, "a thread's own ticks strictly increase");
                        last = now;
                        c.advance_to(now + 1); // a credit never lowers the clock
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // Every tick added 2; advance_to only ever raised. So the floor is THREADS*ITERS*2.
        assert!(c.peek() >= THREADS * ITERS * 2);
    }

    /// P2.8: a threaded process can't fork or execve. `fork` gets fork's real errno
    /// (-EAGAIN) so the guest degrades observably; `execve` (which would kill siblings
    /// and replace the image — a lie to fake) is a fatal `Unsupported` naming the op.
    /// Neither ever panics the host.
    #[test]
    fn threaded_fork_is_eagain_execve_is_fatal() {
        let vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = ThreadCtx {
            tid: 1000,
            clear_tid: 0,
            altstack: Default::default(),
            sigmask: 0,
            robust_list_head: 0,
            robust_list_len: 0,
        };

        cpu.set_reg(Reg::Rax, SYS_FORK);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(matches!(out, SyscallOutcome::Continue));
        assert_eq!(cpu.reg(Reg::Rax), (-11i64) as u64, "fork -> -EAGAIN");

        cpu.set_reg(Reg::Rax, SYS_EXECVE);
        // Null path/argv/envp: read_cstr just returns empty; the point is the outcome.
        cpu.set_reg(Reg::Rdi, 0);
        cpu.set_reg(Reg::Rsi, 0);
        cpu.set_reg(Reg::Rdx, 0);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::Unsupported { what: "execve" }),
            "execve is a fatal, named Unsupported"
        );
    }

    /// A 4 KiB RW page at 0x1000 for the futex/robust-list decode tests.
    fn mt_ctx() -> ThreadCtx {
        ThreadCtx {
            tid: 1000,
            clear_tid: 0,
            altstack: Default::default(),
            sigmask: 0,
            robust_list_head: 0,
            robust_list_len: 0,
        }
    }

    /// task-121: `futex_mt` decodes plain `FUTEX_WAIT`/`FUTEX_WAKE` with a match-any
    /// bitmask (byte-identical to the pre-task-121 outcome) and `FUTEX_WAIT_BITSET`(9)/
    /// `FUTEX_WAKE_BITSET`(10) carrying the `val3` bitmask from R9. The absolute deadline
    /// (WAIT_BITSET) is converted to a relative bound against the virtual clock.
    #[test]
    fn futex_mt_decodes_bitset_ops() {
        const MATCH_ANY: u32 = 0xffff_ffff;
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        // Flip to mt mode so `now_ns` uses the shared virtual clock (the deadline math).
        shim.threaded = true;
        shim.mt_clock.seed(0);
        let mut ctx = mt_ctx();

        // Plain FUTEX_WAIT (op 0), no timeout → match-any bitmask, indefinite.
        cpu.set_reg(Reg::Rax, SYS_FUTEX);
        cpu.set_reg(Reg::Rsi, 0); // FUTEX_WAIT
        cpu.set_reg(Reg::Rdi, 0x1000); // uaddr
        cpu.set_reg(Reg::Rdx, 0); // val
        cpu.set_reg(Reg::R10, 0); // null timeout
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            SyscallOutcome::FutexWait {
                uaddr,
                val,
                timeout,
                bitmask,
            } => {
                assert_eq!(uaddr, 0x1000);
                assert_eq!(val, 0);
                assert_eq!(timeout, None);
                assert_eq!(bitmask, MATCH_ANY, "plain WAIT is match-any");
            }
            _ => panic!("expected FutexWait"),
        }

        // Plain FUTEX_WAKE (op 1) → match-any bitmask.
        cpu.set_reg(Reg::Rsi, 1); // FUTEX_WAKE
        cpu.set_reg(Reg::Rdx, 3); // count
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            SyscallOutcome::FutexWake {
                uaddr,
                count,
                bitmask,
            } => {
                assert_eq!(uaddr, 0x1000);
                assert_eq!(count, 3);
                assert_eq!(bitmask, MATCH_ANY, "plain WAKE is match-any");
            }
            _ => panic!("expected FutexWake"),
        }

        // FUTEX_WAIT_BITSET (op 9): val3 (R9) is the bitmask; R10 an absolute deadline in
        // the future → a positive relative timeout.
        //
        // The deadline MUST be built the way a real guest builds it: glibc calls
        // `clock_gettime(CLOCK_MONOTONIC)` — which our shim serves as `CLOCK_BASE_SEC +
        // now_ns` (tick_clock) — then adds a delta. So the absolute value is base-offset.
        // This is also the 54-year-regression guard (task-121 review): if the base is not
        // subtracted, `deadline_mono ≈ CLOCK_BASE_SEC + 50 ms` and the `<= 50 ms` assert
        // below fails (the old monotonic-no-rebase bug made it an ~indefinite wait).
        let now = shim.mt_clock.peek();
        let deadline_ns = (super::CLOCK_BASE_SEC as u64) * 1_000_000_000 + now + 50_000_000;
        let ts = 0x1100u64;
        vm.write_bytes(ts, &(deadline_ns / 1_000_000_000).to_le_bytes())
            .unwrap();
        vm.write_bytes(ts + 8, &(deadline_ns % 1_000_000_000).to_le_bytes())
            .unwrap();
        cpu.set_reg(Reg::Rsi, 9); // FUTEX_WAIT_BITSET
        cpu.set_reg(Reg::Rdi, 0x1000);
        cpu.set_reg(Reg::Rdx, 0);
        cpu.set_reg(Reg::R10, ts);
        cpu.set_reg(Reg::R9, 0x00c0_ffee); // an arbitrary nonzero bitmask
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            SyscallOutcome::FutexWait {
                timeout, bitmask, ..
            } => {
                assert_eq!(bitmask, 0x00c0_ffee, "WAIT_BITSET carries val3 (R9)");
                let to = timeout.expect("a future deadline yields a relative timeout");
                // now advanced by two now_ns reads is far under 50 ms, so the relative
                // bound is positive and bounded by the original 50 ms window.
                assert!(
                    to > Duration::ZERO && to <= Duration::from_millis(50),
                    "base-offset monotonic deadline must rebase to ~50 ms, got {to:?}"
                );
            }
            _ => panic!("expected FutexWait"),
        }

        // FUTEX_WAKE_BITSET (op 10): val3 (R9) is the bitmask.
        cpu.set_reg(Reg::Rsi, 10);
        cpu.set_reg(Reg::Rdx, 1);
        cpu.set_reg(Reg::R9, 0x0000_0002);
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            SyscallOutcome::FutexWake { bitmask, .. } => {
                assert_eq!(bitmask, 0x0000_0002, "WAKE_BITSET carries val3 (R9)");
            }
            _ => panic!("expected FutexWake"),
        }
    }

    /// task-121: a `FUTEX_WAIT_BITSET` whose absolute deadline is already in the past
    /// decodes to a zero relative timeout — the driver then returns `-ETIMEDOUT` at once
    /// (no negative/huge wait). Also covers the `CLOCK_REALTIME` flag: a realtime absolute
    /// deadline is rebased through `CLOCK_BASE_SEC` onto the same virtual axis.
    #[test]
    fn futex_mt_wait_bitset_past_deadline_is_zero_timeout() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        shim.threaded = true;
        shim.mt_clock.seed(1_000_000_000); // 1 s of virtual monotonic time elapsed
        let mut ctx = mt_ctx();

        // A monotonic absolute deadline of 0 s is in the past (clock is at 1 s).
        let ts = 0x1100u64;
        vm.write_bytes(ts, &0u64.to_le_bytes()).unwrap();
        vm.write_bytes(ts + 8, &0u64.to_le_bytes()).unwrap();
        cpu.set_reg(Reg::Rax, SYS_FUTEX);
        cpu.set_reg(Reg::Rsi, 9); // FUTEX_WAIT_BITSET, CLOCK_MONOTONIC
        cpu.set_reg(Reg::Rdi, 0x1000);
        cpu.set_reg(Reg::Rdx, 0);
        cpu.set_reg(Reg::R10, ts);
        cpu.set_reg(Reg::R9, 0xffff_ffff);
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            SyscallOutcome::FutexWait { timeout, .. } => {
                assert_eq!(
                    timeout,
                    Some(Duration::ZERO),
                    "a past monotonic deadline → zero relative timeout"
                );
            }
            _ => panic!("expected FutexWait"),
        }

        // CLOCK_REALTIME (op | 0x100): a realtime deadline of exactly CLOCK_BASE_SEC maps
        // to virtual monotonic 0, still in the past (clock at 1 s) → zero timeout.
        vm.write_bytes(ts, &super::CLOCK_BASE_SEC.to_le_bytes())
            .unwrap();
        vm.write_bytes(ts + 8, &0u64.to_le_bytes()).unwrap();
        cpu.set_reg(Reg::Rsi, 9 | 0x100); // FUTEX_WAIT_BITSET | FUTEX_CLOCK_REALTIME
        cpu.set_reg(Reg::R10, ts);
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            SyscallOutcome::FutexWait { timeout, .. } => {
                assert_eq!(
                    timeout,
                    Some(Duration::ZERO),
                    "a past realtime deadline (rebased through CLOCK_BASE_SEC) → zero timeout"
                );
            }
            _ => panic!("expected FutexWait"),
        }
    }

    /// task-229: a near-`u64::MAX` timeout timespec pointer must not panic on the
    /// `ts + 8` nsec read. The timespec sits so close to the top of the address space
    /// that `ts + 8` wraps; with a plain `+` this overflow-panics in debug/test builds
    /// (overflow-checks on). `wrapping_add` degrades cleanly: the wrapped, unmapped
    /// address reads back as 0 (via `read_u64`), so `futex_mt` still returns a clean
    /// `FutexWait` — no host panic. Covers both the relative-timeout (`FUTEX_WAIT`) and
    /// absolute-deadline (`FUTEX_WAIT_BITSET`) timespec reads.
    #[test]
    fn futex_mt_near_max_timespec_ptr_no_overflow_panic() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        shim.threaded = true;
        shim.mt_clock.seed(0);
        let mut ctx = mt_ctx();

        // A timespec pointer chosen so `ts + 8` overflows u64 (the sec read is also in the
        // wrap zone). Both are unmapped → read_u64 returns 0 → a clean, indefinite wait.
        let ts = u64::MAX - 3;

        // FUTEX_WAIT: R10 is a *relative* timespec, nsec at `ts + 8`.
        cpu.set_reg(Reg::Rax, SYS_FUTEX);
        cpu.set_reg(Reg::Rsi, 0); // FUTEX_WAIT
        cpu.set_reg(Reg::Rdi, 0x1000);
        cpu.set_reg(Reg::Rdx, 0);
        cpu.set_reg(Reg::R10, ts);
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            SyscallOutcome::FutexWait { timeout, .. } => {
                // sec=0, nsec=0 (both unmapped) → a zero-duration relative timeout.
                assert_eq!(timeout, Some(Duration::ZERO));
            }
            _ => panic!("expected FutexWait"),
        }

        // FUTEX_WAIT_BITSET: R10 is an *absolute* deadline timespec, nsec at `ts + 8`.
        cpu.set_reg(Reg::Rsi, 9); // FUTEX_WAIT_BITSET
        cpu.set_reg(Reg::R9, 0xffff_ffff);
        cpu.set_reg(Reg::R10, ts);
        match shim.handle_mt(&mut cpu, &vm, &mut ctx) {
            // sec=0,nsec=0 is a past absolute deadline → zero relative timeout.
            SyscallOutcome::FutexWait { timeout, .. } => {
                assert_eq!(timeout, Some(Duration::ZERO));
            }
            _ => panic!("expected FutexWait"),
        }
    }

    /// task-121: a `FUTEX_WAIT_BITSET`/`FUTEX_WAKE_BITSET` with a zero `val3` bitmask is
    /// invalid (-EINVAL), matching the kernel — it never reaches the driver.
    #[test]
    fn futex_mt_zero_bitmask_is_einval() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        shim.threaded = true;
        let mut ctx = mt_ctx();

        cpu.set_reg(Reg::Rax, SYS_FUTEX);
        cpu.set_reg(Reg::Rsi, 9); // WAIT_BITSET
        cpu.set_reg(Reg::Rdi, 0x1000);
        cpu.set_reg(Reg::R9, 0); // zero bitmask → invalid
        assert!(matches!(
            shim.handle_mt(&mut cpu, &vm, &mut ctx),
            SyscallOutcome::Continue
        ));
        assert_eq!(
            cpu.reg(Reg::Rax),
            (-22i64) as u64,
            "-EINVAL for zero bitmask"
        );
    }

    /// task-122: `set_robust_list` records the head/len in the caller's `ThreadCtx` (a
    /// per-thread field, like clear_tid) and `get_robust_list` reads them back. A bogus
    /// len is -EINVAL and stores nothing.
    #[test]
    fn robust_list_set_get_roundtrip() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = mt_ctx();

        // set_robust_list(head=0x1234, len=24) → 0, recorded in ctx.
        cpu.set_reg(Reg::Rax, SYS_SET_ROBUST_LIST);
        cpu.set_reg(Reg::Rdi, 0x1234);
        cpu.set_reg(Reg::Rsi, ROBUST_LIST_HEAD_SIZE);
        assert!(matches!(
            shim.handle_mt(&mut cpu, &vm, &mut ctx),
            SyscallOutcome::Continue
        ));
        assert_eq!(cpu.reg(Reg::Rax), 0);
        assert_eq!(ctx.robust_list_head, 0x1234);
        assert_eq!(ctx.robust_list_len, ROBUST_LIST_HEAD_SIZE);

        // get_robust_list(0, head_ptr, len_ptr) writes them back.
        let head_ptr = 0x1100u64;
        let len_ptr = 0x1108u64;
        cpu.set_reg(Reg::Rax, SYS_GET_ROBUST_LIST);
        cpu.set_reg(Reg::Rdi, 0);
        cpu.set_reg(Reg::Rsi, head_ptr);
        cpu.set_reg(Reg::Rdx, len_ptr);
        assert!(matches!(
            shim.handle_mt(&mut cpu, &vm, &mut ctx),
            SyscallOutcome::Continue
        ));
        assert_eq!(cpu.reg(Reg::Rax), 0);
        let mut got_head = [0u8; 8];
        let mut got_len = [0u8; 8];
        vm.read_bytes(head_ptr, &mut got_head).unwrap();
        vm.read_bytes(len_ptr, &mut got_len).unwrap();
        assert_eq!(u64::from_le_bytes(got_head), 0x1234);
        assert_eq!(u64::from_le_bytes(got_len), ROBUST_LIST_HEAD_SIZE);

        // A bogus len is rejected and stores nothing new.
        cpu.set_reg(Reg::Rax, SYS_SET_ROBUST_LIST);
        cpu.set_reg(Reg::Rdi, 0x9999);
        cpu.set_reg(Reg::Rsi, 8); // wrong size
        shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert_eq!(cpu.reg(Reg::Rax), (-22i64) as u64, "bogus len -> -EINVAL");
        assert_eq!(
            ctx.robust_list_head, 0x1234,
            "the head is unchanged on -EINVAL"
        );
    }

    /// The clock is a deterministic virtual tick while single-threaded (each read
    /// advances the ST quantum), then switches to the shared rate-controlled virtual
    /// clock once the process goes threaded (VCLK, decision-6): the flip seeds the mt
    /// clock from `clock_ns` (no backward jump), mt reads tick the mt quantum
    /// monotonically, and a driver-credited wait advances at least its duration.
    #[test]
    fn clock_is_virtual_tick_until_threaded_then_shared_mt_clock() {
        let mut s = LinuxShim::new();
        let a = s.now_ns();
        let b = s.now_ns();
        assert_eq!(
            b - a,
            CLOCK_TICK_NS,
            "single-threaded clock is a fixed tick"
        );

        // Flip to mt mode, seeding the shared clock from the current virtual ns (as the
        // `clone` intercept does). now_ns then ticks the mt quantum, monotonically.
        s.threaded = true;
        s.mt_clock.seed(s.clock_ns);
        let c = s.now_ns();
        let d = s.now_ns();
        assert!(c >= b, "no backward jump across the seed");
        assert_eq!(c - b, MT_CLOCK_TICK_NS, "mt read ticks the mt quantum");
        assert_eq!(d - c, MT_CLOCK_TICK_NS, "mt clock advances monotonically");

        // A driver-credited wait (advance_to entry + duration) jumps the clock forward
        // by at least the wait's duration — the credit-on-expiry path the driver uses.
        let before = s.mt_clock.peek();
        s.mt_clock.advance_to(before + 5_000_000);
        assert!(
            s.now_ns() >= before + 5_000_000,
            "a credited wait advances the clock at least its duration"
        );
    }

    /// P2 (threads): the shim is shared across guest-thread host threads behind
    /// `Arc<Mutex<LinuxShim>>`, so it must be `Send`. The fd table's `Fd` entries hold
    /// `Arc<Mutex<..>>` (not `Rc<RefCell>`) precisely to satisfy this — a regression to
    /// `Rc` would fail to compile here.
    #[test]
    fn shim_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<LinuxShim>();
    }

    /// A `read` whose destination buffer is unmapped must return `-EFAULT`, never
    /// panic the host — guest input can point `read(2)` anywhere (harden: no host
    /// panic from guest input). A Flat VM starts with no mapped regions, so any
    /// guest address is unmapped and `write_bytes` fails.
    #[test]
    fn read_into_unmapped_buffer_efaults_not_panics() {
        let vm = Vm::new(VmConfig::flat(0x1000));
        let mut shim = LinuxShim::new();
        shim.stdin = b"hello".to_vec();
        // fd 0 (stdin) with a destination pointer into unmapped guest memory.
        let r = shim.do_read(&vm, 0, 0x4000, 5);
        assert_eq!(r, EFAULT, "unmapped read buffer must be -EFAULT");
        // A huge length must not abort on allocation either.
        let r = shim.do_read(&vm, 0, 0x4000, usize::MAX);
        assert_eq!(r, EFAULT, "bogus read length must be -EFAULT, not an abort");
    }

    /// Every path a guest can name must resolve inside the rootfs — an untrusted OCI
    /// image can ship symlinks whose targets point at host files (`/leak ->
    /// /etc/passwd`) or climb out with `..`; resolution must contain both.
    #[test]
    fn rootfs_resolution_cannot_escape() {
        // A per-test rootfs plus a sibling "host secret" the guest must never reach.
        let base = std::env::temp_dir().join(format!("x86jit-rootfs-esc-{}", std::process::id()));
        let root = base.join("root");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("etc/passwd"), b"ROOTFS").unwrap();
        std::fs::write(base.join("host_secret"), b"HOSTSECRET").unwrap();
        std::fs::write(root.join("bin/busybox"), b"ELF").unwrap();

        // Attacker symlinks: absolute target, `..`-climbing target, and to the sibling.
        symlink("/etc/passwd", root.join("leak_abs")).unwrap();
        symlink("../../../../etc/passwd", root.join("leak_rel")).unwrap();
        symlink("../host_secret", root.join("leak_host")).unwrap();
        // A legitimate in-root symlink must still resolve.
        symlink("busybox", root.join("bin/cat")).unwrap();

        let inside = |p: &[u8]| {
            let r = resolve_in_rootfs(&root, p).expect("resolves");
            assert!(
                r.starts_with(&root),
                "{:?} escaped the rootfs -> {:?}",
                String::from_utf8_lossy(p),
                r
            );
            r
        };

        // Absolute symlink target re-roots at the rootfs, not host `/`.
        assert_eq!(std::fs::read(inside(b"/leak_abs")).unwrap(), b"ROOTFS");
        // `..`-climbing symlink and literal `..` traversal are clamped.
        inside(b"/leak_rel");
        inside(b"/../../../../etc/passwd");
        assert_eq!(std::fs::read(inside(b"/etc/passwd")).unwrap(), b"ROOTFS");
        // The sibling host file is unreachable (clamped back into the rootfs).
        let leaked = resolve_in_rootfs(&root, b"/leak_host").unwrap();
        assert!(leaked.starts_with(&root), "reached host file via symlink");
        assert_ne!(
            std::fs::read(&leaked).ok().as_deref(),
            Some(b"HOSTSECRET".as_slice()),
            "guest read the host secret"
        );
        // A legitimate in-root symlink resolves to its target.
        assert_eq!(inside(b"/bin/cat"), root.join("bin/busybox"));
        // Sanity: a real host path exists that we must NOT be pointed at.
        assert!(Path::new("/etc/passwd").exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    /// task-162: `uname` fills a plausible utsname, and `readlinkat(/proc/self/exe)`
    /// resolves to the recorded entrypoint path (or `-ENOENT` when unset).
    #[test]
    fn uname_and_readlinkat_self_exe() {
        let mut vm = Vm::new(VmConfig::flat(0x10000));
        vm.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();

        // uname(buf): sysname "Linux", machine "x86_64" (field 4 of char[65]).
        let buf = 0x1000u64;
        cpu.set_reg(Reg::Rax, SYS_UNAME);
        cpu.set_reg(Reg::Rdi, buf);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), 0);
        let mut sysname = [0u8; 6];
        vm.read_bytes(buf, &mut sysname).unwrap();
        assert_eq!(&sysname, b"Linux\0");
        let mut machine = [0u8; 7];
        vm.read_bytes(buf + 65 * 4, &mut machine).unwrap();
        assert_eq!(&machine, b"x86_64\0");

        // readlinkat(AT_FDCWD, "/proc/self/exe", out, 64) → the exe path, no NUL.
        let path_ptr = 0x2000u64;
        let out = 0x2100u64;
        vm.write_bytes(path_ptr, b"/proc/self/exe\0").unwrap();
        cpu.set_reg(Reg::Rdi, (-100i64) as u64); // AT_FDCWD
        cpu.set_reg(Reg::Rsi, path_ptr);
        cpu.set_reg(Reg::Rdx, out);
        cpu.set_reg(Reg::R10, 64);

        shim.exe_path = b"/caddy".to_vec();
        cpu.set_reg(Reg::Rax, SYS_READLINKAT);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), 6); // "/caddy", not NUL-terminated
        let mut got = [0u8; 6];
        vm.read_bytes(out, &mut got).unwrap();
        assert_eq!(&got, b"/caddy");

        // Unset entrypoint → -ENOENT (guest falls back), like plain `readlink`.
        shim.exe_path.clear();
        cpu.set_reg(Reg::Rax, SYS_READLINKAT);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), ENOENT);
    }

    /// task-227 (correctness regression): `handle_mt` must route a `clone` to
    /// `clone_thread` (→ `Spawn`) ONLY for a real *thread* clone (CLONE_VM|CLONE_THREAD).
    /// A `vfork`/`posix_spawn`-shaped clone (CLONE_VM|CLONE_VFORK, no CLONE_THREAD)
    /// reaching an already-threaded process must take the fork path (`fork_eagain` →
    /// `Continue`, `Rax = -EAGAIN`), NOT spawn a sibling host thread over the shared
    /// address space (whose `execve` would corrupt the whole process).
    ///
    /// Before the fix (`SYS_CLONE if Rdi & CLONE_VM != 0 => clone_thread`) the vfork case
    /// misroutes to `clone_thread` and returns `Spawn` — this test asserts `Continue`, so
    /// it goes red without the fix. Verified: reverting the predicate makes the second
    /// assertion fail with `left: Spawn { .. }`.
    #[test]
    fn handle_mt_vfork_shaped_clone_forks_not_spawns() {
        const SYS_CLONE: u64 = 56;
        const EAGAIN: u64 = (-11i64) as u64;
        const CLONE_VM: u64 = 0x100;
        const CLONE_VFORK: u64 = 0x4000;
        const CLONE_THREAD: u64 = 0x0001_0000;

        let vm = Vm::new(VmConfig::flat(0x1000));
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = ThreadCtx {
            tid: shim.pid,
            clear_tid: 0,
            altstack: Default::default(),
            sigmask: 0,
            robust_list_head: 0,
            robust_list_len: 0,
        };

        // Positive control: a real thread clone (CLONE_VM|CLONE_THREAD) → `Spawn`.
        // (`SyscallOutcome` isn't `Debug` — `Spawn` carries a `Box<CpuState>` — so match
        // on the discriminant rather than formatting it.)
        cpu.set_reg(Reg::Rax, SYS_CLONE);
        cpu.set_reg(Reg::Rdi, CLONE_VM | CLONE_THREAD);
        assert!(
            matches!(
                shim.handle_mt(&mut cpu, &vm, &mut ctx),
                SyscallOutcome::Spawn { .. }
            ),
            "real thread clone (CLONE_VM|CLONE_THREAD) must Spawn a sibling"
        );

        // The bug: a vfork/posix_spawn clone (CLONE_VM|CLONE_VFORK, NO CLONE_THREAD)
        // must take the fork path — `Continue` with Rax = -EAGAIN — never `Spawn`.
        cpu.set_reg(Reg::Rax, SYS_CLONE);
        cpu.set_reg(Reg::Rdi, CLONE_VM | CLONE_VFORK);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::Continue),
            "vfork-shaped clone (CLONE_VM|CLONE_VFORK) must fork_eagain, not spawn a \
             thread over the shared address space (its execve would corrupt the process)"
        );
        assert_eq!(
            cpu.reg(Reg::Rax),
            EAGAIN,
            "vfork-shaped clone in a threaded process must return -EAGAIN"
        );
    }

    /// task-130: `sched_getaffinity` reports the real host CPU count so a multi-threaded
    /// guest sees true parallelism (not the old single-CPU answer). Deterministic and
    /// host-count-agnostic: it asserts `popcount(mask) == available_parallelism()`
    /// (clamped to the buffer), so it passes on any CI machine, and checks the return
    /// value follows the `len.max(8)` byte-count contract.
    #[test]
    fn sched_getaffinity_reports_host_cpu_count() {
        let mut vm = Vm::new(VmConfig::flat(0x10000));
        vm.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();

        // Host online CPUs, matching the arm's clamp: [1, 1024] then buffer capacity.
        let host = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        // A generous 128-byte (1024-bit) buffer: no clamping to buffer capacity here.
        let mask = 0x1000u64;
        let cpusetsize = 128usize;
        cpu.set_reg(Reg::Rax, SYS_SCHED_GETAFFINITY);
        cpu.set_reg(Reg::Rdi, 0); // pid 0 = self
        cpu.set_reg(Reg::Rsi, cpusetsize as u64);
        cpu.set_reg(Reg::Rdx, mask);
        assert!(!shim.handle(&mut cpu, &vm));

        // ABI: Rax = bytes written = min(cpusetsize, 128).max(8).
        assert_eq!(cpu.reg(Reg::Rax), cpusetsize.max(8) as u64);

        let mut buf = vec![0u8; cpusetsize];
        vm.read_bytes(mask, &mut buf).unwrap();
        let popcount: u32 = buf.iter().map(|b| b.count_ones()).sum();
        let expected = host.clamp(1, 1024).min(cpusetsize * 8) as u32;
        assert_eq!(
            popcount, expected,
            "online-CPU count must equal host available_parallelism() (clamped)"
        );

        // A tiny buffer (1 byte = 8 bits) must never overflow: the set-bit count is
        // clamped to the buffer capacity, and only that one byte is written.
        let small_mask = 0x1800u64;
        let small = 1usize;
        cpu.set_reg(Reg::Rax, SYS_SCHED_GETAFFINITY);
        cpu.set_reg(Reg::Rsi, small as u64);
        cpu.set_reg(Reg::Rdx, small_mask);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), small.max(8) as u64);
        let mut sbuf = vec![0u8; small];
        vm.read_bytes(small_mask, &mut sbuf).unwrap();
        let spop: u32 = sbuf.iter().map(|b| b.count_ones()).sum();
        assert_eq!(spop, host.clamp(1, 1024).min(small * 8) as u32);
    }

    /// task-125 (inline path): a threaded `read` of a pipe that already has data is served
    /// **inline** (`Continue`, `Rax` = bytes read) — no yield when data is ready, exactly
    /// like the epoll `timeout==0` fast path. Proves we don't park unnecessarily.
    #[test]
    fn read_mt_serves_ready_pipe_inline() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = mt_ctx();

        // A pipe read end with 3 bytes already buffered and a live writer.
        let buf = Arc::new(Mutex::new(PipeBuf::with(
            VecDeque::from(b"abc".to_vec()),
            1,
            1,
        )));
        shim.fs.fd_table.insert(7, Fd::PipeRead(buf));

        cpu.set_reg(Reg::Rax, SYS_READ);
        cpu.set_reg(Reg::Rdi, 7);
        cpu.set_reg(Reg::Rsi, 0x1000);
        cpu.set_reg(Reg::Rdx, 8);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::Continue),
            "ready data serves inline, no yield"
        );
        assert_eq!(cpu.reg(Reg::Rax), 3, "3 bytes read");
        let mut got = [0u8; 3];
        vm.read_bytes(0x1000, &mut got).unwrap();
        assert_eq!(&got, b"abc");
    }

    /// task-125 (would-block probe): a threaded `read` of an *empty* pipe that still has a
    /// live writer yields `BlockingRead` (the driver parks it), while an empty pipe with no
    /// writers is served inline as EOF (`Continue`, `Rax` = 0). This pins the inline-vs-yield
    /// decision at its two boundaries.
    #[test]
    fn read_mt_empty_pipe_yields_only_with_a_live_writer() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = mt_ctx();

        // Empty pipe, live writer → would block → yield.
        let live = Arc::new(Mutex::new(PipeBuf::with(VecDeque::new(), 1, 1)));
        shim.fs.fd_table.insert(7, Fd::PipeRead(live));
        cpu.set_reg(Reg::Rax, SYS_READ);
        cpu.set_reg(Reg::Rdi, 7);
        cpu.set_reg(Reg::Rsi, 0x1000);
        cpu.set_reg(Reg::Rdx, 8);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::BlockingRead { .. }),
            "empty pipe + live writer must yield BlockingRead"
        );

        // Empty pipe, no writers → EOF, served inline.
        let eof = Arc::new(Mutex::new(PipeBuf::with(VecDeque::new(), 0, 1)));
        shim.fs.fd_table.insert(8, Fd::PipeRead(eof));
        cpu.set_reg(Reg::Rdi, 8);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::Continue),
            "empty pipe, no writer is EOF, served inline"
        );
        assert_eq!(cpu.reg(Reg::Rax), 0, "EOF reads 0");
    }

    /// task-232: a threaded `read` of an *empty* pipe read end set to O_NONBLOCK (with a
    /// live writer) is served **inline** with `-EAGAIN` — never a `BlockingRead` park —
    /// while an otherwise identical *blocking* read end still yields. The self-pipe /
    /// event-loop contract: an idle drain returns immediately, it doesn't hang the loop.
    #[test]
    fn read_mt_nonblocking_empty_pipe_returns_eagain_inline() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = mt_ctx();

        // Empty pipe, live writer, O_NONBLOCK read end → inline -EAGAIN, no yield.
        let mut nb = PipeBuf::with(VecDeque::new(), 1, 1);
        nb.nonblocking = true;
        shim.fs
            .fd_table
            .insert(7, Fd::PipeRead(Arc::new(Mutex::new(nb))));
        cpu.set_reg(Reg::Rax, SYS_READ);
        cpu.set_reg(Reg::Rdi, 7);
        cpu.set_reg(Reg::Rsi, 0x1000);
        cpu.set_reg(Reg::Rdx, 8);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::Continue),
            "a nonblocking empty pipe must serve inline, not park"
        );
        assert_eq!(
            cpu.reg(Reg::Rax) as i64,
            -11,
            "empty nonblocking pipe with a live writer → -EAGAIN"
        );

        // The same shape but *blocking* still yields BlockingRead (unchanged). Re-set Rax
        // (the -EAGAIN above overwrote the syscall number the handler reads).
        let live = Arc::new(Mutex::new(PipeBuf::with(VecDeque::new(), 1, 1)));
        shim.fs.fd_table.insert(8, Fd::PipeRead(live));
        cpu.set_reg(Reg::Rax, SYS_READ);
        cpu.set_reg(Reg::Rdi, 8);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::BlockingRead { .. }),
            "a blocking empty pipe with a live writer still yields"
        );
    }

    /// task-232: `pipe2(O_NONBLOCK)` marks the read end nonblocking, and `fcntl(F_SETFL)`
    /// toggles it on a pipe read fd (with F_GETFL reading it back) — the two ways a guest
    /// arms a self-pipe. Drives the real syscall arms so the flag plumbing is covered.
    #[test]
    fn pipe2_and_fcntl_track_nonblock_on_pipe_read_end() {
        const F_GETFL: u64 = 3;
        const F_SETFL: u64 = 4;
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();

        // pipe2(fds, O_NONBLOCK): the read end (fds[0]) is nonblocking.
        cpu.set_reg(Reg::Rax, SYS_PIPE2);
        cpu.set_reg(Reg::Rdi, 0x1000);
        cpu.set_reg(Reg::Rsi, libc::O_NONBLOCK as u64);
        assert!(!shim.handle(&mut cpu, &vm));
        let mut fds = [0u8; 8];
        vm.read_bytes(0x1000, &mut fds).unwrap();
        let rfd = u32::from_le_bytes(fds[0..4].try_into().unwrap()) as u64;

        // F_GETFL reports O_NONBLOCK back.
        cpu.set_reg(Reg::Rax, SYS_FCNTL);
        cpu.set_reg(Reg::Rdi, rfd);
        cpu.set_reg(Reg::Rsi, F_GETFL);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(
            cpu.reg(Reg::Rax) & (libc::O_NONBLOCK as u64),
            libc::O_NONBLOCK as u64,
            "pipe2(O_NONBLOCK) read end reports nonblocking via F_GETFL"
        );

        // F_SETFL(0) clears it; F_GETFL now reports blocking.
        cpu.set_reg(Reg::Rax, SYS_FCNTL);
        cpu.set_reg(Reg::Rdi, rfd);
        cpu.set_reg(Reg::Rsi, F_SETFL);
        cpu.set_reg(Reg::Rdx, 0);
        assert!(!shim.handle(&mut cpu, &vm));
        cpu.set_reg(Reg::Rax, SYS_FCNTL); // F_SETFL returned 0; restore the syscall nr
        cpu.set_reg(Reg::Rsi, F_GETFL);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(
            cpu.reg(Reg::Rax) & (libc::O_NONBLOCK as u64),
            0,
            "F_SETFL(0) clears O_NONBLOCK on the pipe read end"
        );
    }

    /// task-125 (resume): `read_ready` — the driver's post-block completion — drains the
    /// held pipe target straight into guest memory, independent of any fd number (a sibling
    /// may have closed the guest fd while we were parked). The heart of the yield+resume.
    #[test]
    fn read_ready_drains_pipe_target() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut shim = LinuxShim::new();

        let buf = Arc::new(Mutex::new(PipeBuf::with(
            VecDeque::from(b"hello".to_vec()),
            1,
            1,
        )));
        let target = ReadTarget::Pipe(buf);
        // A pipe target never blocks a host syscall, so it always completes (`Some`).
        let n = shim.read_ready(&vm, &target, 0x1000, 16);
        assert_eq!(n, Some(5), "all buffered bytes read");
        let mut got = [0u8; 5];
        vm.read_bytes(0x1000, &mut got).unwrap();
        assert_eq!(&got, b"hello");
    }

    /// task-230 (the lost-readiness race, whole-process-deadlock class): the driver's
    /// `read_target_ready` probe is level-triggered, so a single data event on a *shared,
    /// blocking-mode* host fd wakes two parked reader threads. The first re-takes the shim
    /// lock and drains the fd; the second re-takes the lock and calls `read_ready` — at
    /// which point the fd is empty. The pre-fix `read_ready` did a raw **blocking**
    /// `libc::read` here, which blocks on the empty blocking fd *while holding the shim
    /// lock*, stalling every sibling → whole-process deadlock.
    ///
    /// This test reproduces the loser's exact state: a blocking-mode socketpair whose data
    /// was already drained (a stand-in for "the winner got it"). It asserts `read_ready`
    /// returns `None` (re-park) instead of blocking. **How it confirms the pre-fix hang:**
    /// the fd is blocking-mode (`fd_is_nonblocking` is false — no `O_NONBLOCK` set) and
    /// empty, so the old `libc::read(h, …)` in the `Host` arm would block indefinitely with
    /// no writer left; the test thread would never return. We run the call on a spawned
    /// thread with a bounded `join`-by-timeout: the fixed code returns `None` within
    /// microseconds; the pre-fix code would exceed the bound (it is parked in `read`).
    #[test]
    fn read_ready_lost_race_reparks_not_blocks() {
        // A connected, blocking-mode socketpair. Nothing is written to `a`, so it is empty
        // — exactly the loser's view after the winner drained the one ready event.
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let _b = unsafe { OwnedFd::from_raw_fd(fds[1]) }; // keep the peer open (no EOF)

        // Sanity: the fd is blocking-mode, so a raw `libc::read` on it *would* block —
        // proving the pre-fix hazard is real (the fix's poll-guard is what avoids it).
        assert!(
            !fd_is_nonblocking(a.as_raw_fd()),
            "socketpair endpoints are blocking-mode by default"
        );

        let target = ReadTarget::Host(Arc::new(a));
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut shim = LinuxShim::new();

        // Run the completion on a worker with a bounded wait. The fixed code polls the empty
        // fd, sees not-readable, and returns `None` immediately; the pre-fix code blocks in
        // `libc::read` forever → the recv times out (which we treat as the deadlock).
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let got = shim.read_ready(&vm, &target, 0x1000, 16);
            let _ = tx.send(got);
        });
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("read_ready must return promptly (pre-fix it blocks in libc::read → hang)");
        assert_eq!(got, None, "the lost-race loser must re-park, not read");
        worker.join().unwrap();
    }

    /// task-230 (the happy path still completes): when the shared host fd *is* readable at
    /// completion time (the winner, or a lone reader), `read_ready` does the real `read` and
    /// returns `Some(bytes)` — the poll-guard fences only the empty-fd case, it doesn't
    /// regress a genuine ready read.
    #[test]
    fn read_ready_host_ready_completes() {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        // Peer writes 5 bytes → `a` is genuinely readable.
        let n = unsafe { libc::write(b.as_raw_fd(), b"world".as_ptr() as *const libc::c_void, 5) };
        assert_eq!(n, 5);

        let target = ReadTarget::Host(Arc::new(a));
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut shim = LinuxShim::new();

        let got = shim.read_ready(&vm, &target, 0x1000, 16);
        assert_eq!(got, Some(5), "a ready host fd completes normally");
        let mut buf = [0u8; 5];
        vm.read_bytes(0x1000, &mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    /// task-230 (accept lost-race): the accept analogue — a blocking-mode listen socket with
    /// **no** pending connection is the loser's view after a sibling accepted the one peer
    /// that arrived. The pre-fix `accept_ready` did a raw **blocking** `libc::accept4` here,
    /// blocking on the peer-less listen fd *while holding the shim lock* → whole-process
    /// deadlock. The fixed `accept_ready` polls first, sees no connection, and returns `None`
    /// (re-park). Confirmed the same way as the read case: the listen fd is blocking-mode and
    /// has no backlog, so the old `accept4` would block forever; a bounded `recv` proves the
    /// fix returns promptly.
    #[test]
    fn accept_ready_lost_race_reparks_not_blocks() {
        // A bound+listening TCP socket on an ephemeral port with no client → no pending
        // connection. Blocking-mode (we don't set O_NONBLOCK), so a raw accept4 would block.
        let listen = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(listen >= 0, "socket");
        let listen = unsafe { OwnedFd::from_raw_fd(listen) };
        let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        sa.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
        sa.sin_port = 0; // ephemeral
        let r = unsafe {
            libc::bind(
                listen.as_raw_fd(),
                &sa as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        assert_eq!(r, 0, "bind");
        assert_eq!(unsafe { libc::listen(listen.as_raw_fd(), 8) }, 0, "listen");
        assert!(
            !fd_is_nonblocking(listen.as_raw_fd()),
            "the listen fd is blocking-mode → a raw accept4 would block"
        );

        let raw = listen.as_raw_fd();
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut shim = LinuxShim::new();
        let _keep = listen; // keep the fd alive for the raw handle

        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let got = shim.accept_ready(&vm, raw, 0, 0, 0);
            let _ = tx.send(got);
        });
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("accept_ready must return promptly (pre-fix it blocks in accept4 → hang)");
        assert_eq!(got, None, "the lost-race loser must re-park, not accept");
        worker.join().unwrap();
    }

    /// task-233 (would-block probe): a threaded `recvfrom` on a *blocking-mode* socket with no
    /// data ready must yield `BlockingRecv` (the driver parks it outside the shim lock), not
    /// issue an inline blocking `libc::recvfrom` under the lock (the deadlock class). Setup: a
    /// blocking-mode socketpair endpoint with nothing queued; the peer stays open so it isn't
    /// EOF. Then a peer send makes the fd readable, and the driver's completion (`recv_ready`)
    /// delivers the data — proving the yield resolves.
    #[test]
    fn recvfrom_mt_blocking_empty_yields_then_completes() {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let b = unsafe { OwnedFd::from_raw_fd(fds[1]) }; // keep the peer open (no EOF)
        assert!(
            !fd_is_nonblocking(a.as_raw_fd()),
            "socketpair endpoints are blocking-mode by default"
        );

        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = mt_ctx();
        shim.fs.fd_table.insert(7, Fd::Socket(Arc::new(a)));

        // Empty blocking-mode socket → must yield, not block inline.
        cpu.set_reg(Reg::Rax, SYS_RECVFROM);
        cpu.set_reg(Reg::Rdi, 7);
        cpu.set_reg(Reg::Rsi, 0x1000);
        cpu.set_reg(Reg::Rdx, 16);
        cpu.set_reg(Reg::R10, 0); // flags
        cpu.set_reg(Reg::R8, 0); // src_addr NULL
        cpu.set_reg(Reg::R9, 0); // addrlen NULL
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        let req = match out {
            SyscallOutcome::BlockingRecv(req) => req,
            _ => panic!("blocking-mode empty recvfrom must yield BlockingRecv"),
        };

        // Peer sends: the socket is now readable, so the driver's completion delivers the data.
        let n = unsafe { libc::write(b.as_raw_fd(), b"hi!".as_ptr() as *const libc::c_void, 3) };
        assert_eq!(n, 3);
        let got = shim.recv_ready(&vm, &req);
        assert_eq!(got, Some(3), "recv_ready completes with the peer's bytes");
        let mut buf = [0u8; 3];
        vm.read_bytes(0x1000, &mut buf).unwrap();
        assert_eq!(&buf, b"hi!");
    }

    /// task-233 (Go-netpoller unaffected): a threaded `recvfrom` on a *nonblocking* socket is
    /// served **inline** (`Continue`), returning the real `-EAGAIN` the netpoller polls for —
    /// never a park. This is the exact O_NONBLOCK guard that keeps Go/caddy immune.
    #[test]
    fn recvfrom_mt_nonblocking_stays_inline_eagain() {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let _b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        // Put `a` in O_NONBLOCK, as the Go netpoller does for its sockets.
        let fl = unsafe { libc::fcntl(a.as_raw_fd(), libc::F_GETFL) };
        assert!(fl >= 0);
        assert_eq!(
            unsafe { libc::fcntl(a.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK) },
            0
        );
        assert!(fd_is_nonblocking(a.as_raw_fd()), "now nonblocking");

        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = mt_ctx();
        shim.fs.fd_table.insert(7, Fd::Socket(Arc::new(a)));

        cpu.set_reg(Reg::Rax, SYS_RECVFROM);
        cpu.set_reg(Reg::Rdi, 7);
        cpu.set_reg(Reg::Rsi, 0x1000);
        cpu.set_reg(Reg::Rdx, 16);
        cpu.set_reg(Reg::R10, 0);
        cpu.set_reg(Reg::R8, 0);
        cpu.set_reg(Reg::R9, 0);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::Continue),
            "a nonblocking recvfrom must serve inline, never park"
        );
        const EAGAIN: u64 = (-11i64) as u64;
        const EWOULDBLOCK: u64 = (-11i64) as u64;
        let rax = cpu.reg(Reg::Rax);
        assert!(
            rax == EAGAIN || rax == EWOULDBLOCK,
            "empty nonblocking recvfrom returns -EAGAIN, got {}",
            rax as i64
        );
    }

    /// task-233 review: `MSG_DONTWAIT` is a per-call nonblock flag — a `recvfrom` with it set
    /// on a *blocking-mode* socket (no O_NONBLOCK) must serve inline with -EAGAIN, NOT park.
    /// Without the flag check the empty blocking socket yields `BlockingRecv` and hangs; this
    /// asserts `Continue`, so it goes red without the fix.
    #[test]
    fn recvfrom_mt_msg_dontwait_on_blocking_socket_stays_inline_eagain() {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let _b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        // `a` stays BLOCKING-mode (no O_NONBLOCK) — MSG_DONTWAIT alone must make it inline.
        assert!(!fd_is_nonblocking(a.as_raw_fd()), "socket is blocking-mode");

        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let mut ctx = mt_ctx();
        shim.fs.fd_table.insert(7, Fd::Socket(Arc::new(a)));

        cpu.set_reg(Reg::Rax, SYS_RECVFROM);
        cpu.set_reg(Reg::Rdi, 7);
        cpu.set_reg(Reg::Rsi, 0x1000);
        cpu.set_reg(Reg::Rdx, 16);
        cpu.set_reg(Reg::R10, libc::MSG_DONTWAIT as u64); // the per-call nonblock flag
        cpu.set_reg(Reg::R8, 0);
        cpu.set_reg(Reg::R9, 0);
        let out = shim.handle_mt(&mut cpu, &vm, &mut ctx);
        assert!(
            matches!(out, SyscallOutcome::Continue),
            "MSG_DONTWAIT recvfrom on a blocking socket must serve inline, never park"
        );
        const EAGAIN: u64 = (-11i64) as u64;
        assert_eq!(
            cpu.reg(Reg::Rax),
            EAGAIN,
            "empty MSG_DONTWAIT recvfrom returns -EAGAIN"
        );
    }

    /// task-233 (the lost-readiness race, whole-process-deadlock class): the driver's
    /// `fd_readable` probe is level-triggered, so one datagram on a *shared, blocking-mode*
    /// socket wakes two parked receiver threads. The first re-takes the shim lock and drains
    /// the socket; the second re-takes the lock and calls `recv_ready` — at which point the
    /// socket is empty. Without the poll-under-lock guard, `recv_ready` would do a raw
    /// **blocking** `libc::recvfrom` on the empty blocking fd *while holding the shim lock*,
    /// stalling every sibling → whole-process deadlock.
    ///
    /// This reproduces the loser's exact state: a blocking-mode socketpair already drained.
    /// It asserts `recv_ready` returns `None` (re-park) instead of blocking. **How it confirms
    /// the pre-fix hang:** the fd is blocking-mode and empty with the peer still open, so a raw
    /// `libc::recvfrom` would block forever; we run the call on a worker with a bounded `recv`
    /// — the fixed code polls, sees not-readable, returns `None` in microseconds; neutralizing
    /// the guard (a raw recvfrom) exceeds the bound (parked in the syscall).
    #[test]
    fn recv_ready_lost_race_reparks_not_blocks() {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let _b = unsafe { OwnedFd::from_raw_fd(fds[1]) }; // keep the peer open (no EOF)
        assert!(
            !fd_is_nonblocking(a.as_raw_fd()),
            "socketpair endpoints are blocking-mode by default"
        );

        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
        let mut shim = LinuxShim::new();
        let req = BlockingRecv {
            fd: Arc::new(a),
            flags: 0,
            kind: RecvKind::Recvfrom {
                buf: 0x1000,
                len: 16,
                src: 0,
                addrlen_ptr: 0,
            },
        };

        // Run the completion on a worker with a bounded wait. The fixed code polls the empty
        // fd, sees not-readable, and returns `None`; the pre-fix code blocks in `libc::recvfrom`
        // forever → the recv times out (which we treat as the deadlock).
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let got = shim.recv_ready(&vm, &req);
            let _ = tx.send(got);
        });
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("recv_ready must return promptly (pre-fix it blocks in libc::recvfrom → hang)");
        assert_eq!(got, None, "the lost-race loser must re-park, not recv");
        worker.join().unwrap();
    }

    /// task-233 (the happy path still completes): when the shared socket *is* readable at
    /// completion time (the winner, or a lone receiver), `recv_ready` does the real recv and
    /// returns `Some(bytes)` — the poll-guard fences only the empty-fd case, it doesn't regress
    /// a genuine ready recv. Exercises the `Recvmsg` flavor too (iovec scatter under the
    /// completion path), so the control/scatter writeback is proven byte-identical there.
    #[test]
    fn recv_ready_recvmsg_ready_scatters() {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let n = unsafe {
            libc::write(
                b.as_raw_fd(),
                b"scatter!".as_ptr() as *const libc::c_void,
                8,
            )
        };
        assert_eq!(n, 8);

        let mut vm = Vm::with_backend(VmConfig::flat(0x3000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        let mut shim = LinuxShim::new();

        // Build a guest msghdr at 0x1000 with two iovecs (5 + 8 bytes) at 0x1100/0x1200, so the
        // 8 received bytes scatter across both segments (5 into the first, 3 into the second).
        let msgp = 0x1000u64;
        let iov0 = 0x1400u64; // iovec array (2 entries × 16 bytes)
        let buf0 = 0x1100u64;
        let buf1 = 0x1200u64;
        // msghdr: name(0,0) at +0/+8, iov at +16, iovlen at +24, control(0) +32, controllen(0) +40
        vm.write_bytes(msgp + 16, &iov0.to_le_bytes()).unwrap();
        vm.write_bytes(msgp + 24, &2u64.to_le_bytes()).unwrap();
        vm.write_bytes(msgp + 32, &0u64.to_le_bytes()).unwrap();
        vm.write_bytes(msgp + 40, &0u64.to_le_bytes()).unwrap();
        // iovec[0] = { base: buf0, len: 5 }, iovec[1] = { base: buf1, len: 8 }
        vm.write_bytes(iov0, &buf0.to_le_bytes()).unwrap();
        vm.write_bytes(iov0 + 8, &5u64.to_le_bytes()).unwrap();
        vm.write_bytes(iov0 + 16, &buf1.to_le_bytes()).unwrap();
        vm.write_bytes(iov0 + 24, &8u64.to_le_bytes()).unwrap();

        let req = BlockingRecv {
            fd: Arc::new(a),
            flags: 0,
            kind: RecvKind::Recvmsg { msgp },
        };
        let got = shim.recv_ready(&vm, &req);
        assert_eq!(got, Some(8), "a ready socket completes normally");
        let mut s0 = [0u8; 5];
        let mut s1 = [0u8; 3];
        vm.read_bytes(buf0, &mut s0).unwrap();
        vm.read_bytes(buf1, &mut s1).unwrap();
        assert_eq!(&s0, b"scatt", "first 5 bytes into segment 1");
        assert_eq!(&s1, b"er!", "remaining 3 bytes into segment 2");
    }

    /// task-231 (multi-segment readv, same lock-held-blocking class): a `readv` whose first
    /// segment is exactly filled and whose second segment would block must NOT issue a
    /// second **blocking** `libc::read` under the shim lock — it stops the scatter and
    /// returns the bytes from segment 1 (a short `readv` is POSIX-legal; the guest reissues).
    /// Setup: a blocking-mode socketpair with exactly 4 bytes queued and a two-segment iovec
    /// (seg 1 = 4 bytes, seg 2 = 8 bytes). Pre-fix, segment 1 fills exactly (`n == seg_len`)
    /// so the loop issues `do_read` for segment 2 on the now-empty blocking fd → blocks under
    /// the lock. Post-fix, the `fd_readable` guard before segment 2 sees not-readable and
    /// stops, returning 4. A bounded worker+`recv` proves it returns rather than hangs.
    #[test]
    fn readv_mt_second_segment_would_block_short_reads() {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(r, 0, "socketpair");
        let a = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let b = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        assert!(!fd_is_nonblocking(a.as_raw_fd()), "blocking-mode endpoint");
        // Exactly 4 bytes queued: fills segment 1 exactly, leaving the fd empty for seg 2.
        let n = unsafe { libc::write(b.as_raw_fd(), b"abcd".as_ptr() as *const libc::c_void, 4) };
        assert_eq!(n, 4);

        let mut vm = Vm::with_backend(VmConfig::flat(0x3000), Box::new(InterpreterBackend));
        vm.map(0x1000, 0x2000, Prot::RW, RegionKind::Ram).unwrap();
        let mut shim = LinuxShim::new();
        // Install the socket at guest fd 7.
        shim.fs.fd_table.insert(7, Fd::Socket(Arc::new(a)));
        let _keep_peer = b; // hold the peer open so `a` never sees EOF (which wouldn't block)

        // Build a two-segment iovec in guest memory: seg1 = (0x1800, 4), seg2 = (0x1810, 8).
        let iov = 0x1000u64;
        vm.write_bytes(iov, &0x1800u64.to_le_bytes()).unwrap();
        vm.write_bytes(iov + 8, &4u64.to_le_bytes()).unwrap();
        vm.write_bytes(iov + 16, &0x1810u64.to_le_bytes()).unwrap();
        vm.write_bytes(iov + 24, &8u64.to_le_bytes()).unwrap();

        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rax, SYS_READV);
        cpu.set_reg(Reg::Rdi, 7);
        cpu.set_reg(Reg::Rsi, iov);
        cpu.set_reg(Reg::Rdx, 2);

        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let out = shim.readv_mt(&mut cpu, &vm);
            let _ = tx.send((matches!(out, SyscallOutcome::Continue), cpu.reg(Reg::Rax)));
        });
        let (is_continue, rax) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("readv_mt must return promptly (pre-fix seg-2 read blocks under lock → hang)");
        assert!(is_continue, "the readv is served inline");
        assert_eq!(
            rax, 4,
            "short read of segment 1 only; no blocking seg-2 read"
        );
        worker.join().unwrap();
    }

    /// task-131: `madvise(MADV_DONTNEED)` on a written, host-mapped (`MAP_NORESERVE`) RAM
    /// range must read back as **zero** afterward — the load-bearing Go-scavenger guarantee
    /// (task-161). The host-madvise path (which frees RSS) must still satisfy it: the
    /// released anonymous pages refault as zero. Multi-page span with page-aligned edges, so
    /// the whole range is covered by the host madvise (no reliance on the edge-zero fallback).
    #[test]
    fn madvise_dontneed_zeroes_host_mapped_range() {
        const SPAN: u64 = 1 << 20; // 1 MiB NORESERVE span
        let ram = hostmem::reserve(SPAN);
        let mut vm =
            Vm::with_backend_host_ram(VmConfig::reserved(SPAN), Box::new(InterpreterBackend), ram);
        // Map a 3-page RAM region and dirty it with a non-zero pattern across all pages.
        let base = 0x1000u64;
        let len = 0x3000u64; // 3 host pages
        vm.map(base, len as usize, Prot::RW, RegionKind::Ram)
            .unwrap();
        let pattern = vec![0xABu8; len as usize];
        vm.write_bytes(base, &pattern).unwrap();
        let mut before = vec![0u8; len as usize];
        vm.read_bytes(base, &mut before).unwrap();
        assert!(before.iter().all(|&b| b == 0xAB), "range dirtied first");

        // Sanity: this IS a host-mapped RAM range (the host-madvise branch is taken).
        assert!(
            vm.mem.host_ram_ptr(base, len as usize).is_some(),
            "the fixture must be host-mapped so the madvise passthrough runs"
        );

        // madvise(base, len, MADV_DONTNEED).
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        cpu.set_reg(Reg::Rax, SYS_MADVISE);
        cpu.set_reg(Reg::Rdi, base);
        cpu.set_reg(Reg::Rsi, len);
        cpu.set_reg(Reg::Rdx, 4); // MADV_DONTNEED
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), 0, "madvise returns 0");

        // The SACRED postcondition: every byte reads back zero.
        let mut after = vec![0xFFu8; len as usize];
        vm.read_bytes(base, &mut after).unwrap();
        assert!(
            after.iter().all(|&b| b == 0),
            "MADV_DONTNEED range must read back zero (task-161 guarantee)"
        );
    }

    /// task-131: unaligned edges. The host madvise only covers whole pages; the partial
    /// bytes below the first full page and above the last full page must still be zeroed by
    /// the explicit write-path fallback, so the *entire* asked range reads back zero.
    #[test]
    fn madvise_dontneed_zeroes_partial_edge_pages() {
        const SPAN: u64 = 1 << 20;
        let ram = hostmem::reserve(SPAN);
        let mut vm =
            Vm::with_backend_host_ram(VmConfig::reserved(SPAN), Box::new(InterpreterBackend), ram);
        // Map a region and dirty a range whose start/end are NOT page-aligned:
        // [0x1800, 0x4800) — half of page 0x1000, all of 0x2000/0x3000, half of 0x4000.
        vm.map(0x1000, 0x5000, Prot::RW, RegionKind::Ram).unwrap();
        let addr = 0x1800u64;
        let len = 0x3000u64;
        vm.write_bytes(addr, &vec![0xCDu8; len as usize]).unwrap();

        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        cpu.set_reg(Reg::Rax, SYS_MADVISE);
        cpu.set_reg(Reg::Rdi, addr);
        cpu.set_reg(Reg::Rsi, len);
        cpu.set_reg(Reg::Rdx, 4);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), 0);

        let mut after = vec![0xFFu8; len as usize];
        vm.read_bytes(addr, &mut after).unwrap();
        assert!(
            after.iter().all(|&b| b == 0),
            "unaligned edges must be zeroed too, not just the inner full pages"
        );
        // And a byte just BELOW the range (still 0x11.. of the first page, outside the
        // asked range) must be untouched — we never spill the madvise onto it.
        vm.write_bytes(0x1000, &[0x77u8]).unwrap();
        cpu.set_reg(Reg::Rax, SYS_MADVISE);
        cpu.set_reg(Reg::Rdi, addr);
        cpu.set_reg(Reg::Rsi, len);
        cpu.set_reg(Reg::Rdx, 4);
        assert!(!shim.handle(&mut cpu, &vm));
        let mut edge = [0u8; 1];
        vm.read_bytes(0x1000, &mut edge).unwrap();
        assert_eq!(
            edge[0], 0x77,
            "a byte the guest didn't ask about is untouched"
        );
    }

    /// task-131: on a **Vec-backed** `Reserved` VM (no host mapping to madvise), the arm
    /// falls back to the explicit write-zero path — the range must still read back zero.
    #[test]
    fn madvise_dontneed_zeroes_vec_backed_range() {
        // `VmConfig::flat` uses a Vec (Owner::Boxed) backing — no host mmap.
        let mut vm = Vm::with_backend(VmConfig::flat(0x10000), Box::new(InterpreterBackend));
        let base = 0x1000u64;
        let len = 0x3000u64;
        vm.map(base, len as usize, Prot::RW, RegionKind::Ram)
            .unwrap();
        vm.write_bytes(base, &vec![0x5Au8; len as usize]).unwrap();
        // No host mapping → host_ram_ptr is None → write-zero fallback is exercised.
        assert!(
            vm.mem.host_ram_ptr(base, len as usize).is_none(),
            "flat/Vec backing has no host mapping"
        );

        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        cpu.set_reg(Reg::Rax, SYS_MADVISE);
        cpu.set_reg(Reg::Rdi, base);
        cpu.set_reg(Reg::Rsi, len);
        cpu.set_reg(Reg::Rdx, 4);
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), 0);

        let mut after = vec![0xFFu8; len as usize];
        vm.read_bytes(base, &mut after).unwrap();
        assert!(
            after.iter().all(|&b| b == 0),
            "Vec-backed DONTNEED must still zero via the write fallback"
        );
    }

    /// task-131: a non-DONTNEED advice (e.g. MADV_WILLNEED=3) stays a no-op success — it
    /// returns 0 and does NOT touch guest memory.
    #[test]
    fn madvise_non_dontneed_is_noop_success() {
        let mut vm = Vm::with_backend(VmConfig::flat(0x10000), Box::new(InterpreterBackend));
        let base = 0x1000u64;
        let len = 0x2000u64;
        vm.map(base, len as usize, Prot::RW, RegionKind::Ram)
            .unwrap();
        vm.write_bytes(base, &vec![0x99u8; len as usize]).unwrap();

        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        cpu.set_reg(Reg::Rax, SYS_MADVISE);
        cpu.set_reg(Reg::Rdi, base);
        cpu.set_reg(Reg::Rsi, len);
        cpu.set_reg(Reg::Rdx, 3); // MADV_WILLNEED — not DONTNEED
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(cpu.reg(Reg::Rax), 0, "advice still succeeds");

        let mut after = vec![0u8; len as usize];
        vm.read_bytes(base, &mut after).unwrap();
        assert!(
            after.iter().all(|&b| b == 0x99),
            "non-DONTNEED advice must not touch memory"
        );
    }

    /// task-131 review: a `madvise(MADV_DONTNEED)` with a top-page address near `u64::MAX`
    /// must not abort the host. Rounding `addr` up to a page boundary can overflow `u64`
    /// even when `addr + len` does not; without the `checked_mul` guard the inner-page
    /// arithmetic panics in debug builds (the default test profile) from fully
    /// guest-controlled RDI. Best-effort no-op, `Rax = 0`, no panic (harden #1).
    #[test]
    fn madvise_dontneed_top_page_addr_does_not_abort() {
        let vm = Vm::with_backend(VmConfig::flat(0x10000), Box::new(InterpreterBackend));
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        cpu.set_reg(Reg::Rax, SYS_MADVISE);
        cpu.set_reg(Reg::Rdi, u64::MAX - 0xFFF); // top host page: div_ceil rounds past u64::MAX
        cpu.set_reg(Reg::Rsi, 0); // len 0 (addr+len does not overflow)
        cpu.set_reg(Reg::Rdx, 4); // MADV_DONTNEED
        assert!(!shim.handle(&mut cpu, &vm));
        assert_eq!(
            cpu.reg(Reg::Rax),
            0,
            "best-effort no-op, never a host abort"
        );
    }

    /// Drive an anonymous `mmap(len)` through `handle`, returning the guest address.
    fn do_mmap(shim: &mut LinuxShim, cpu: &mut Vcpu, vm: &Vm, len: u64) -> u64 {
        cpu.set_reg(Reg::Rax, SYS_MMAP);
        cpu.set_reg(Reg::Rdi, 0); // addr = NULL
        cpu.set_reg(Reg::Rsi, len);
        cpu.set_reg(Reg::Rdx, 0x3); // PROT_READ|WRITE
        cpu.set_reg(Reg::R10, 0x22); // MAP_PRIVATE|MAP_ANONYMOUS (no MAP_FIXED)
        cpu.set_reg(Reg::R8, (-1i64) as u64); // fd = -1 → anonymous
        cpu.set_reg(Reg::R9, 0); // offset
        assert!(!shim.handle(cpu, vm));
        cpu.reg(Reg::Rax)
    }

    fn do_munmap(shim: &mut LinuxShim, cpu: &mut Vcpu, vm: &Vm, addr: u64, len: u64) {
        cpu.set_reg(Reg::Rax, SYS_MUNMAP);
        cpu.set_reg(Reg::Rdi, addr);
        cpu.set_reg(Reg::Rsi, len);
        assert!(!shim.handle(cpu, vm));
        assert_eq!(cpu.reg(Reg::Rax), 0, "munmap always succeeds");
    }

    /// task-124: `munmap` reclaims arena space, so a mmap/munmap loop doesn't grow the
    /// bump past its high-water mark, and a reused span reads back zero.
    #[test]
    fn munmap_reclaims_arena_no_unbounded_growth() {
        let mut vm = Vm::new(VmConfig::flat(0x100000));
        vm.map(0x10000, 0x80000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        shim.mmap_base = 0x10000;
        shim.mmap_limit = 0x10000 + 0x80000;

        let len = 0x4000u64; // 16 KiB (page-aligned)

        // Prime the high-water mark with a few live spans, then note the peak.
        let a = do_mmap(&mut shim, &mut cpu, &vm, len);
        let b = do_mmap(&mut shim, &mut cpu, &vm, len);
        let c = do_mmap(&mut shim, &mut cpu, &vm, len);
        assert!(a != 0 && b != 0 && c != 0);
        // Dirty the middle span so a later reuse must be re-zeroed to read back clean.
        vm.write_bytes(b, &[0xABu8; 0x4000]).unwrap();
        // Free the middle span only (NOT the top → it lands on the free list).
        do_munmap(&mut shim, &mut cpu, &vm, b, len);
        let peak = shim.mmap_base;

        // Churn: repeatedly mmap+munmap the same size. The bump must not advance past
        // the peak — every allocation comes from the reclaimed span.
        for _ in 0..1000 {
            let p = do_mmap(&mut shim, &mut cpu, &vm, len);
            assert_ne!(p, 0, "arena must not ENOMEM while reusing freed space");
            assert!(
                shim.mmap_base <= peak,
                "bump advanced past the high-water mark → space not reclaimed"
            );
            // A reused span must read back zero like a fresh anonymous map.
            let mut buf = [0xFFu8; 0x4000];
            vm.read_bytes(p, &mut buf).unwrap();
            assert!(
                buf.iter().all(|&x| x == 0),
                "reused mmap must read back zero"
            );
            do_munmap(&mut shim, &mut cpu, &vm, p, len);
        }
    }

    /// task-124: freeing the TOP of the bump rolls `mmap_base` back; freeing the span
    /// just below rolls it back further (cascade through the coalesced free list).
    #[test]
    fn munmap_top_of_bump_rolls_back() {
        let vm = Vm::new(VmConfig::flat(0x100000));
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let start = 0x20000u64;
        shim.mmap_base = start;
        shim.mmap_limit = start + 0x80000;
        let len = 0x2000u64;

        let a = do_mmap(&mut shim, &mut cpu, &vm, len);
        let b = do_mmap(&mut shim, &mut cpu, &vm, len);
        assert_eq!(a, start);
        assert_eq!(b, start + len);
        assert_eq!(shim.mmap_base, start + 2 * len);

        // Free B (the top) → base rolls back to A's end.
        do_munmap(&mut shim, &mut cpu, &vm, b, len);
        assert_eq!(
            shim.mmap_base,
            start + len,
            "top-of-bump munmap rolls base back"
        );

        // Free A (now the top) → base rolls all the way back to the start.
        do_munmap(&mut shim, &mut cpu, &vm, a, len);
        assert_eq!(
            shim.mmap_base, start,
            "second munmap unwinds to the arena start"
        );
    }

    /// task-124 regression: a span re-bumped into space a top-of-bump `munmap` rolled
    /// back over must read back ZERO. Before the high-water fix, the rollback path
    /// re-handed dirty bytes (musl-CPython read stale metadata → heap corruption).
    #[test]
    fn munmap_rollback_rebump_reads_zero() {
        let mut vm = Vm::new(VmConfig::flat(0x100000));
        vm.map(0x20000, 0x10000, Prot::RW, RegionKind::Ram).unwrap();
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let start = 0x20000u64;
        shim.mmap_base = start;
        shim.mmap_limit = start + 0x10000;
        let len = 0x2000u64;

        let a = do_mmap(&mut shim, &mut cpu, &vm, len);
        // Dirty the span, then free it at the top of the bump (rolls base back).
        vm.write_bytes(a, &[0xCDu8; 0x2000]).unwrap();
        do_munmap(&mut shim, &mut cpu, &vm, a, len);
        assert_eq!(shim.mmap_base, start, "top munmap rolled the bump back");

        // Re-bump into the rolled-back region: the same address, but it must read zero.
        let reused = do_mmap(&mut shim, &mut cpu, &vm, len);
        assert_eq!(reused, a, "re-bumps into the reclaimed region");
        let mut buf = [0xFFu8; 0x2000];
        vm.read_bytes(reused, &mut buf).unwrap();
        assert!(
            buf.iter().all(|&x| x == 0),
            "a re-bumped rolled-back span must read back zero"
        );
    }

    /// task-124: a non-top free lands on the free list; freeing the span above it, then
    /// the (now-top) live span, cascades the coalesced free spans back into the bump.
    #[test]
    fn munmap_free_list_coalesces_and_cascades() {
        let vm = Vm::new(VmConfig::flat(0x100000));
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let start = 0x20000u64;
        shim.mmap_base = start;
        shim.mmap_limit = start + 0x80000;
        let len = 0x1000u64;

        let a = do_mmap(&mut shim, &mut cpu, &vm, len);
        let b = do_mmap(&mut shim, &mut cpu, &vm, len);
        let c = do_mmap(&mut shim, &mut cpu, &vm, len);
        let d = do_mmap(&mut shim, &mut cpu, &vm, len);
        assert_eq!(shim.mmap_base, start + 4 * len);

        // Free B and C (neither is the top) → they coalesce into one free span.
        do_munmap(&mut shim, &mut cpu, &vm, b, len);
        do_munmap(&mut shim, &mut cpu, &vm, c, len);
        assert_eq!(
            shim.mmap_base,
            start + 4 * len,
            "non-top frees don't move base"
        );

        // Free D (the top): base rolls back over D and then folds the coalesced B+C
        // free span (now abutting the top) back into the bump too.
        do_munmap(&mut shim, &mut cpu, &vm, d, len);
        assert_eq!(
            shim.mmap_base,
            start + len,
            "cascade unwinds D + the free B/C span; only A stays"
        );
        assert_eq!(a, start);
    }

    /// task-124: a partial `munmap` (unmapping only part of a tracked span) frees just
    /// that sub-range and keeps the rest live — the remainder is not double-freed and
    /// stays out of the free list.
    #[test]
    fn munmap_partial_span_splits() {
        let vm = Vm::new(VmConfig::flat(0x100000));
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let start = 0x20000u64;
        shim.mmap_base = start;
        shim.mmap_limit = start + 0x80000;

        let a = do_mmap(&mut shim, &mut cpu, &vm, 0x4000); // 4 pages
        assert_eq!(a, start);
        assert_eq!(shim.mmap_base, start + 0x4000);

        // Unmap the middle two pages [a+0x1000, a+0x3000): prefix and suffix stay live.
        do_munmap(&mut shim, &mut cpu, &vm, a + 0x1000, 0x2000);
        // The freed middle is on the free list; a same-size alloc must reuse it (bump
        // must not advance).
        let peak = shim.mmap_base;
        let reused = do_mmap(&mut shim, &mut cpu, &vm, 0x2000);
        assert_eq!(reused, a + 0x1000, "the freed middle page range is reused");
        assert_eq!(shim.mmap_base, peak, "reuse must not grow the bump");
    }

    /// task-124: `munmap` of an address the shim never handed out (or a MAP_FIXED /
    /// file-backed region we don't track) just succeeds with no accounting change.
    #[test]
    fn munmap_untracked_address_is_noop() {
        let vm = Vm::new(VmConfig::flat(0x100000));
        let mut cpu = vm.new_vcpu();
        let mut shim = LinuxShim::new();
        let start = 0x20000u64;
        shim.mmap_base = start;
        shim.mmap_limit = start + 0x80000;

        let a = do_mmap(&mut shim, &mut cpu, &vm, 0x2000);
        let base_after = shim.mmap_base;
        // An unrelated address (never allocated) — munmap succeeds, base unchanged.
        do_munmap(&mut shim, &mut cpu, &vm, 0x70000, 0x1000);
        assert_eq!(
            shim.mmap_base, base_after,
            "untracked munmap changes nothing"
        );
        // The live span is untouched: re-freeing it still rolls the bump back.
        do_munmap(&mut shim, &mut cpu, &vm, a, 0x2000);
        assert_eq!(shim.mmap_base, start, "the real span still reclaims");
    }
}
