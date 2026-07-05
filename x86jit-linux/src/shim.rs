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

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::PathBuf;
use std::rc::Rc;

use x86jit_core::{Reg, Vcpu, Vm};

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
const SYS_CHMOD: u64 = 90;
const SYS_FCHMOD: u64 = 91;
const SYS_CHOWN: u64 = 92;
const SYS_FCHOWN: u64 = 93;
const SYS_UNLINKAT: u64 = 263;
const SYS_SET_ROBUST_LIST: u64 = 273;
const SYS_PRLIMIT64: u64 = 302;
const SYS_GETRANDOM: u64 = 318;
const SYS_RSEQ: u64 = 334;
const SYS_FUTEX: u64 = 202;
const SYS_NEWFSTATAT: u64 = 262;

const ENOENT: u64 = (-2i64) as u64;
const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;
const SYS_BRK: u64 = 12;
const SYS_RT_SIGACTION: u64 = 13;
const SYS_RT_SIGPROCMASK: u64 = 14;
const SYS_IOCTL: u64 = 16;
const SYS_READV: u64 = 19;
const SYS_WRITEV: u64 = 20;
const SYS_ACCESS: u64 = 21;
const SYS_GETPID: u64 = 39;
const SYS_FCNTL: u64 = 72;
const SYS_GETCWD: u64 = 79;
const SYS_READLINK: u64 = 89;
const SYS_GETTID: u64 = 186;
const SYS_GETDENTS64: u64 = 217;
const SYS_TIME: u64 = 201;
const SYS_GETTIMEOFDAY: u64 = 96;
const SYS_CLOCK_GETTIME: u64 = 228;
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

const ENOTTY: u64 = (-25i64) as u64;
const ENOMEM: u64 = (-12i64) as u64;

const O_ACCMODE: u64 = 0o3;
const O_RDONLY: u64 = 0;
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;

/// `-EACCES` / `-ENOENT` etc. as the kernel returns them: a small negative in RAX.
const EACCES: u64 = (-13i64) as u64;
const EBADF: u64 = (-9i64) as u64;
const ENOSYS: u64 = (-38i64) as u64;

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
/// live behind `Rc<RefCell<..>>` so a `dup`/`dup2` alias shares the seek offset
/// (POSIX). Single-threaded deferred model — `Rc`, not `Arc`.
enum Fd {
    Stdin,
    Stdout,
    Stderr,
    File(Rc<RefCell<OpenEntry>>),
    PipeRead(Rc<RefCell<PipeBuf>>),
    PipeWrite(Rc<RefCell<PipeBuf>>),
}

/// A pipe's shared byte buffer. **Unbounded** (a writer never blocks): the deferred,
/// single-threaded process model runs a writer to completion before its reader, so
/// pipe backpressure never arises (documented limitation, oci-multiprocess-plan.md
/// §2). `writers`/`readers` count the open ends so a read past the last writer sees
/// EOF (a drained buffer already reads as EOF here).
struct PipeBuf {
    data: VecDeque<u8>,
    writers: usize,
    readers: usize,
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
    fn file(&self, fd: u64) -> Option<Rc<RefCell<OpenEntry>>> {
        match self.fd_table.get(&fd) {
            Some(Fd::File(rc)) => Some(rc.clone()),
            _ => None,
        }
    }

    /// The pipe buffer behind `fd` if it's the read end.
    fn pipe_read(&self, fd: u64) -> Option<Rc<RefCell<PipeBuf>>> {
        match self.fd_table.get(&fd) {
            Some(Fd::PipeRead(rc)) => Some(rc.clone()),
            _ => None,
        }
    }

    /// Would a `read(fd)` block? True only for a pipe read end whose buffer is empty
    /// while a writer is still open — the case the scheduler resolves by running a
    /// pending writer child. An empty pipe with no writers is EOF (returns 0), not a
    /// block.
    fn pipe_would_block(&self, fd: u64) -> bool {
        match self.fd_table.get(&fd) {
            Some(Fd::PipeRead(rc)) => {
                let b = rc.borrow();
                b.data.is_empty() && b.writers > 0
            }
            _ => false,
        }
    }

    /// Lowest free fd ≥ 3 (dup2 may plant entries at arbitrary numbers, so scan).
    fn alloc_fd(&self) -> u64 {
        let mut fd = 3;
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
    /// Bytes the guest reads from fd 0 (stdin). A file-DB CLI reads its script here.
    pub stdin: Vec<u8>,
    stdin_pos: usize,
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
        Self::default()
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
    pub fn fork(&self) -> LinuxShim {
        let mut fd_table = BTreeMap::new();
        for (&fd, entry) in &self.fs.fd_table {
            let dup = match entry {
                Fd::Stdin => Fd::Stdin,
                Fd::Stdout => Fd::Stdout,
                Fd::Stderr => Fd::Stderr,
                Fd::File(rc) => Fd::File(rc.clone()),
                Fd::PipeRead(rc) => {
                    rc.borrow_mut().readers += 1;
                    Fd::PipeRead(rc.clone())
                }
                Fd::PipeWrite(rc) => {
                    rc.borrow_mut().writers += 1;
                    Fd::PipeWrite(rc.clone())
                }
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
            stdin: self.stdin.clone(),
            stdin_pos: self.stdin_pos,
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
        }
    }

    /// Complete a `read` the scheduler parked (see [`Self::pending_read`]) after
    /// running pending writer children. Drains whatever is now in the pipe; an empty
    /// buffer reads as EOF (0), so a spurious wake can't loop forever.
    pub fn resume_read(&mut self, vm: &mut Vm, fd: u64, buf: u64, len: usize) -> u64 {
        self.do_read(vm, fd, buf, len)
    }

    /// Close every fd this process holds — called when the process exits so a pipe's
    /// writer/reader counts fall to zero and the other end sees EOF (POSIX: exit
    /// closes all descriptors).
    pub fn close_all_fds(&mut self) {
        for (_, entry) in std::mem::take(&mut self.fs.fd_table) {
            match entry {
                Fd::PipeRead(rc) => {
                    let mut b = rc.borrow_mut();
                    b.readers = b.readers.saturating_sub(1);
                }
                Fd::PipeWrite(rc) => {
                    let mut b = rc.borrow_mut();
                    b.writers = b.writers.saturating_sub(1);
                }
                _ => {}
            }
        }
    }

    /// Drop `fd` from the table, decrementing a pipe end's open count so a reader
    /// can see EOF once the last writer closes. Returns whether the fd existed.
    fn release(&mut self, fd: u64) -> bool {
        match self.fs.fd_table.remove(&fd) {
            Some(Fd::PipeRead(rc)) => {
                let mut b = rc.borrow_mut();
                b.readers = b.readers.saturating_sub(1);
                true
            }
            Some(Fd::PipeWrite(rc)) => {
                let mut b = rc.borrow_mut();
                b.writers = b.writers.saturating_sub(1);
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    /// Handle one `Exit::Syscall`. Returns `true` when the program has exited.
    pub fn handle(&mut self, cpu: &mut Vcpu, vm: &mut Vm) -> bool {
        let nr = cpu.reg(Reg::Rax);
        match nr {
            SYS_WRITE => {
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let mut data = vec![0u8; len];
                vm.read_bytes(buf, &mut data)
                    .expect("write buffer is mapped");
                let ret = match self.fs.fd_table.get(&fd) {
                    Some(Fd::Stdout) => {
                        self.stdout.extend_from_slice(&data);
                        len as u64
                    }
                    Some(Fd::Stderr) => {
                        self.stderr.extend_from_slice(&data);
                        len as u64
                    }
                    // A writable passthrough file: append at the current position.
                    Some(Fd::File(rc)) => match rc.borrow_mut().as_file_mut() {
                        Some(f) => match f.write(&data) {
                            Ok(n) => n as u64,
                            Err(_) => EBADF,
                        },
                        None => len as u64,
                    },
                    Some(Fd::PipeWrite(rc)) => {
                        rc.borrow_mut().data.extend(data.iter().copied());
                        len as u64
                    }
                    Some(Fd::PipeRead(_)) => EBADF, // write to the read end
                    // stdin or an unknown fd: swallow (matches prior behavior).
                    Some(Fd::Stdin) | None => len as u64,
                };
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
                for i in 0..cnt {
                    let base = read_u64(vm, iov + i * 16);
                    let len = read_u64(vm, iov + i * 16 + 8) as usize;
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
                for i in 0..cnt {
                    let base = read_u64(vm, iov + i * 16);
                    let len = read_u64(vm, iov + i * 16 + 8) as usize;
                    if len == 0 {
                        continue; // kernel ignores empty segments (base may be null)
                    }
                    let mut data = vec![0u8; len];
                    vm.read_bytes(base, &mut data).expect("iovec buffer mapped");
                    match self.fs.fd_table.get(&fd) {
                        Some(Fd::Stdout) => self.stdout.extend_from_slice(&data),
                        Some(Fd::Stderr) => self.stderr.extend_from_slice(&data),
                        // A passthrough file: append at the current position.
                        Some(Fd::File(rc)) => {
                            if let Some(f) = rc.borrow_mut().as_file_mut() {
                                let _ = f.write_all(&data);
                            }
                        }
                        Some(Fd::PipeWrite(rc)) => rc.borrow_mut().data.extend(data.iter().copied()),
                        Some(Fd::PipeRead(_)) | Some(Fd::Stdin) | None => {}
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
                    if self.mmap_base != 0 && self.mmap_base + aligned <= self.mmap_limit {
                        let a = self.mmap_base;
                        self.mmap_base += aligned;
                        a
                    } else {
                        cpu.set_reg(Reg::Rax, ENOMEM);
                        return false;
                    }
                };
                if fd >= 0 {
                    // File-backed: copy the file's bytes in (the tail past EOF stays
                    // zero, since guest RAM is zero-initialized).
                    if let Some(rc) = self.fs.file(fd as u64) {
                        let entry = rc.borrow();
                        if let Some(file) = entry.as_file() {
                            let mut scratch = vec![0u8; len as usize];
                            if let Ok(n) = file.read_at(&mut scratch, off) {
                                vm.write_bytes(target, &scratch[..n])
                                    .expect("mmap target mapped");
                            }
                        }
                    }
                } else if flags & MAP_FIXED != 0 {
                    // Anonymous MAP_FIXED (a segment's bss) must present zeroed pages,
                    // overwriting whatever a prior file mapping left there.
                    let _ = vm.write_bytes(target, &vec![0u8; len as usize]);
                }
                cpu.set_reg(Reg::Rax, target);
                false
            }
            SYS_MUNMAP | SYS_MPROTECT => {
                // No-op: the bump allocator never frees, and page protections aren't
                // enforced in the flat model (§4.2).
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
                let meta = self.fs.file(fd).and_then(|rc| rc.borrow().metadata());
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
                    Some(rc) => match rc.borrow().as_file() {
                        Some(file) => {
                            let mut scratch = vec![0u8; len];
                            match file.read_at(&mut scratch, off) {
                                Ok(n) => {
                                    vm.write_bytes(buf, &scratch[..n])
                                        .expect("pread buffer mapped");
                                    n as u64
                                }
                                Err(_) => EBADF,
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
                        .and_then(|rc| rc.borrow().metadata())
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
                // Report an 8 MiB soft stack limit, unlimited hard, if `old` given.
                let old = cpu.reg(Reg::R10);
                if old != 0 {
                    let mut buf = [0u8; 16];
                    buf[0..8].copy_from_slice(&(8u64 * 1024 * 1024).to_le_bytes());
                    buf[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
                    let _ = vm.write_bytes(old, &buf);
                }
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_GETRANDOM => {
                // Fixed bytes → deterministic; glibc uses this for its pointer guard.
                let buf = cpu.reg(Reg::Rdi);
                let len = cpu.reg(Reg::Rsi) as usize;
                let _ = vm.write_bytes(buf, &vec![0x42u8; len]);
                cpu.set_reg(Reg::Rax, len as u64);
                false
            }
            SYS_IOCTL => {
                // No ttys in the harness → isatty() reports false.
                cpu.set_reg(Reg::Rax, ENOTTY);
                false
            }
            SYS_RT_SIGPROCMASK | SYS_RT_SIGACTION => {
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_FUTEX => {
                // futex(uaddr, op, val, ...). Single-threaded harness: a WAKE is a
                // no-op, and a WAIT can only be a lost race — if the word already
                // differs from `val` return -EAGAIN like the kernel; if it still
                // matches, no other thread exists to change it, so blocking would
                // deadlock. Panic loudly instead of hanging the test.
                const FUTEX_CMD_MASK: u64 = 0x7f; // strip PRIVATE/CLOCK flags
                const FUTEX_WAIT: u64 = 0;
                let op = cpu.reg(Reg::Rsi) & FUTEX_CMD_MASK;
                let ret = if op == FUTEX_WAIT {
                    let uaddr = cpu.reg(Reg::Rdi);
                    let val = cpu.reg(Reg::Rdx) as u32;
                    let mut w = [0u8; 4];
                    vm.read_bytes(uaddr, &mut w).expect("futex word mapped");
                    if u32::from_le_bytes(w) == val {
                        panic!("FUTEX_WAIT would block forever (single-threaded guest, *{uaddr:#x} == {val:#x})");
                    }
                    (-11i64) as u64 // -EAGAIN: the word changed before we slept
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
                let ret = match self.fs.file(fd) {
                    Some(rc) => match rc.borrow_mut().as_file_mut() {
                        Some(f) => {
                            let pos = match whence {
                                0 => std::io::SeekFrom::Start(off as u64),
                                1 => std::io::SeekFrom::Current(off),
                                _ => std::io::SeekFrom::End(off),
                            };
                            match std::io::Seek::seek(f, pos) {
                                Ok(p) => p,
                                Err(_) => (-29i64) as u64, // -ESPIPE
                            }
                        }
                        None => (-9i64) as u64, // -EBADF
                    },
                    None => (-9i64) as u64, // -EBADF
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_PWRITE64 => {
                // pwrite(fd, buf, len, off): positioned write, file offset untouched.
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let off = cpu.reg(Reg::R10);
                let mut data = vec![0u8; len];
                vm.read_bytes(buf, &mut data).expect("pwrite buffer mapped");
                let ret = match self.fs.file(fd) {
                    Some(rc) => match rc.borrow().as_file() {
                        Some(f) => match f.write_at(&data, off) {
                            Ok(n) => n as u64,
                            Err(_) => EBADF,
                        },
                        None => EBADF,
                    },
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_FTRUNCATE => {
                let fd = cpu.reg(Reg::Rdi);
                let size = cpu.reg(Reg::Rsi);
                let ret = match self.fs.file(fd) {
                    Some(rc) => match rc.borrow().as_file() {
                        Some(f) => match f.set_len(size) {
                            Ok(()) => 0,
                            Err(_) => EBADF,
                        },
                        None => EBADF,
                    },
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_PIPE | SYS_PIPE2 => {
                // pipe(fds) / pipe2(fds, flags): allocate one shared buffer, hand out
                // a read end and a write end, and write the two fd numbers to the
                // guest `int[2]` at RDI. pipe2 flags (O_CLOEXEC/O_NONBLOCK) are
                // ignored for now — cloexec matters only once execve preserves fds
                // (oci-multiprocess-plan.md §4), which is a later rung.
                let ptr = cpu.reg(Reg::Rdi);
                let pipe = Rc::new(RefCell::new(PipeBuf {
                    data: VecDeque::new(),
                    writers: 1,
                    readers: 1,
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
                let dup = match self.fs.fd_table.get(&old) {
                    Some(Fd::Stdin) => Some(Fd::Stdin),
                    Some(Fd::Stdout) => Some(Fd::Stdout),
                    Some(Fd::Stderr) => Some(Fd::Stderr),
                    Some(Fd::File(rc)) => Some(Fd::File(rc.clone())),
                    Some(Fd::PipeRead(rc)) => {
                        rc.borrow_mut().readers += 1; // the alias is another open read end
                        Some(Fd::PipeRead(rc.clone()))
                    }
                    Some(Fd::PipeWrite(rc)) => {
                        rc.borrow_mut().writers += 1;
                        Some(Fd::PipeWrite(rc.clone()))
                    }
                    None => None,
                };
                let ret = if old == new {
                    // dup2(fd, fd) is a no-op that returns fd — but only if fd is valid.
                    if dup.is_some() { new } else { EBADF }
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
                    if let Some(f) = rc.borrow_mut().as_file_mut() {
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
                // F_SETFD/F_SETLK/F_GETFL etc. — benign: succeed / report O_RDONLY.
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_GETPID | SYS_GETTID => {
                cpu.set_reg(Reg::Rax, 1000);
                false
            }
            SYS_TIME => {
                let t = 1_700_000_000u64; // fixed epoch → deterministic
                let tloc = cpu.reg(Reg::Rdi);
                if tloc != 0 {
                    let _ = vm.write_bytes(tloc, &t.to_le_bytes());
                }
                cpu.set_reg(Reg::Rax, t);
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
            SYS_GETDENTS64 => {
                // Stream `struct linux_dirent64` records for an open directory into
                // the guest buffer until it's full; 0 when exhausted. An
                // interpreter's importer lists directories to discover modules.
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let count = cpu.reg(Reg::Rdx) as usize;
                let mut out = Vec::new();
                if let Some(rc) = self.fs.file(fd) {
                    if let OpenEntry::Dir(d) = &mut *rc.borrow_mut() {
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
                // Fixed epoch → deterministic. timespec { i64 sec, i64 nsec } at RSI.
                let mut ts = [0u8; 16];
                ts[0..8].copy_from_slice(&1_700_000_000i64.to_le_bytes());
                let _ = vm.write_bytes(cpu.reg(Reg::Rsi), &ts);
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_GETTIMEOFDAY => {
                // timeval { i64 sec, i64 usec } at RDI.
                let mut tv = [0u8; 16];
                tv[0..8].copy_from_slice(&1_700_000_000i64.to_le_bytes());
                let _ = vm.write_bytes(cpu.reg(Reg::Rdi), &tv);
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
                const CLONE_VM: u64 = 0x100;
                if cpu.reg(Reg::Rdi) & CLONE_VM != 0 {
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
                        .insert(fd, Fd::File(Rc::new(RefCell::new(OpenEntry::File(f)))));
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
            .insert(fd, Fd::File(Rc::new(RefCell::new(entry))));
        fd
    }

    /// Resolve a guest `read`: pull bytes from the host file into a scratch buffer,
    /// then copy them into guest memory. Returns the byte count or a negative errno.
    fn do_read(&mut self, vm: &mut Vm, fd: u64, buf: u64, len: usize) -> u64 {
        // A passthrough file takes precedence — a tool can `dup2` its input onto
        // fd 0 and then read "stdin" (busybox gunzip does exactly this).
        if let Some(rc) = self.fs.file(fd) {
            let mut entry = rc.borrow_mut();
            let Some(file) = entry.as_file_mut() else {
                return EBADF;
            };
            let mut scratch = vec![0u8; len];
            return match file.read(&mut scratch) {
                Ok(n) => {
                    vm.write_bytes(buf, &scratch[..n])
                        .expect("read buffer is mapped");
                    n as u64
                }
                Err(_) => EBADF,
            };
        }
        if let Some(rc) = self.fs.pipe_read(fd) {
            // Drain up to `len` bytes; an empty buffer reads as EOF (0). The deferred
            // model runs the writer to completion first, so the data is already here.
            let chunk: Vec<u8> = {
                let mut b = rc.borrow_mut();
                let n = len.min(b.data.len());
                b.data.drain(..n).collect()
            };
            vm.write_bytes(buf, &chunk).expect("pipe read buffer mapped");
            return chunk.len() as u64;
        }
        if fd == 0 {
            // Real stdin: drain the scripted buffer, EOF (0) once exhausted.
            let n = len.min(self.stdin.len() - self.stdin_pos);
            let chunk = self.stdin[self.stdin_pos..self.stdin_pos + n].to_vec();
            vm.write_bytes(buf, &chunk).expect("stdin buffer mapped");
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

/// Write a minimal x86-64 `struct stat` (144 bytes) describing `meta` as a regular
/// file: enough for the size/mode checks a hashing utility makes. `st_dev`/`st_ino`
/// carry the real host values — glibc's ld.so dedupes loaded objects by that pair,
/// so a fabricated (0, 0) would collide with the main map and make it treat
/// `libc.so.6` as already loaded.
fn write_stat(vm: &mut Vm, addr: u64, meta: &std::fs::Metadata) {
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

/// Write a `struct stat` describing a character device (for stdin/stdout/stderr).
fn write_chr_stat(vm: &mut Vm, addr: u64) {
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
    let mut out = Vec::new();
    for _ in 0..4096 {
        let mut b = [0u8; 1];
        if vm.read_bytes(addr, &mut b).is_err() || b[0] == 0 {
            break;
        }
        out.push(b[0]);
        addr += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::resolve_in_rootfs;
    use std::os::unix::fs::symlink;
    use std::path::Path;

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
}
