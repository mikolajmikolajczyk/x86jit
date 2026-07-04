//! Test-side Linux x86-64 syscall shim (testing.md §9). The core never emulates
//! an OS (§1); this is a thin embedder that reacts to `Exit::Syscall`.
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

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use x86jit_core::{Reg, Vcpu, Vm};

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_STAT: u64 = 4;
const SYS_FSTAT: u64 = 5;
const SYS_LSEEK: u64 = 8;
const SYS_PREAD64: u64 = 17;
const SYS_SET_ROBUST_LIST: u64 = 273;
const SYS_PRLIMIT64: u64 = 302;
const SYS_GETRANDOM: u64 = 318;
const SYS_RSEQ: u64 = 334;
const SYS_NEWFSTATAT: u64 = 262;

const ENOENT: u64 = (-2i64) as u64;
const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;
const SYS_BRK: u64 = 12;
const SYS_RT_SIGACTION: u64 = 13;
const SYS_RT_SIGPROCMASK: u64 = 14;
const SYS_IOCTL: u64 = 16;
const SYS_WRITEV: u64 = 20;
const SYS_ACCESS: u64 = 21;
const SYS_GETPID: u64 = 39;
const SYS_FCNTL: u64 = 72;
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
const SYS_EXIT_GROUP: u64 = 231;
const ARCH_SET_FS: u64 = 0x1002;

const ENOTTY: u64 = (-25i64) as u64;
const ENOMEM: u64 = (-12i64) as u64;

const O_ACCMODE: u64 = 0o3;
const O_RDONLY: u64 = 0;

/// `-EACCES` / `-ENOENT` etc. as the kernel returns them: a small negative in RAX.
const EACCES: u64 = (-13i64) as u64;
const EBADF: u64 = (-9i64) as u64;

/// Deterministic responses for syscalls beyond the built-ins, keyed by number
/// (testing.md §9). Keeps whole-program tests reproducible when a program issues
/// a syscall whose real effect we don't model — return a scripted value.
#[derive(Default)]
pub struct ScriptedSyscalls {
    pub responses: Vec<(u64, u64)>,
}

impl ScriptedSyscalls {
    fn get(&self, nr: u64) -> Option<u64> {
        self.responses.iter().find(|(n, _)| *n == nr).map(|(_, r)| *r)
    }
}

/// Read-only host filesystem passthrough (testing.md §12). Disabled unless an
/// allowlist is installed; only exact paths on it may be opened, and only
/// `O_RDONLY`. Guest fds we hand out start at 3 and index `open_files` — a guest
/// can only `read`/`close` a descriptor this shim itself opened, never an
/// arbitrary host fd.
#[derive(Default)]
struct FsPassthrough {
    allow: Vec<PathBuf>,
    /// `(path suffix, host file)`: any guest open of a path ending in the suffix
    /// (and not a `glibc-hwcaps` variant) is served from the host file. Lets a
    /// dynamic loader find e.g. `libc.so.6` from a checked-in fixture regardless of
    /// the machine-specific absolute path baked into the binary.
    serve: Vec<(Vec<u8>, PathBuf)>,
    open_files: HashMap<u64, File>,
    next_fd: u64,
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
    fs: FsPassthrough,
}

impl LinuxShim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Permit read-only host passthrough for exactly the given path (testing.md
    /// §12). Any `open` of a path not permitted returns `-ENOENT`.
    pub fn allow_read(&mut self, path: impl Into<PathBuf>) {
        self.fs.next_fd = self.fs.next_fd.max(3);
        self.fs.allow.push(path.into());
    }

    /// Serve `host` for any guest `open` of a path ending in `suffix` (except
    /// `glibc-hwcaps` probe variants). Lets a dynamic loader find a shared library
    /// (`libc.so.6`) from a checked-in fixture regardless of the absolute path
    /// baked into the binary.
    pub fn serve_lib(&mut self, suffix: impl Into<Vec<u8>>, host: impl Into<PathBuf>) {
        self.fs.next_fd = self.fs.next_fd.max(3);
        self.fs.serve.push((suffix.into(), host.into()));
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
                vm.read_bytes(buf, &mut data).expect("write buffer is mapped");
                match fd {
                    1 => self.stdout.extend_from_slice(&data),
                    2 => self.stderr.extend_from_slice(&data),
                    _ => {}
                }
                cpu.set_reg(Reg::Rax, len as u64);
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
                let ret = self.do_read(vm, fd, buf, len);
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_CLOSE => {
                let fd = cpu.reg(Reg::Rdi);
                let ret = if self.fs.open_files.remove(&fd).is_some() { 0 } else { EBADF };
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
            SYS_WRITEV => {
                // writev(fd, iov, iovcnt): gather the iovec array and write it.
                let fd = cpu.reg(Reg::Rdi);
                let iov = cpu.reg(Reg::Rsi);
                let cnt = cpu.reg(Reg::Rdx);
                let mut total = 0u64;
                for i in 0..cnt {
                    let base = read_u64(vm, iov + i * 16);
                    let len = read_u64(vm, iov + i * 16 + 8) as usize;
                    let mut data = vec![0u8; len];
                    vm.read_bytes(base, &mut data).expect("iovec buffer mapped");
                    match fd {
                        1 => self.stdout.extend_from_slice(&data),
                        2 => self.stderr.extend_from_slice(&data),
                        _ => {}
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
                let fd = cpu.reg(Reg::R8);
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
                if fd != u64::MAX {
                    // File-backed: copy the file's bytes in (the tail past EOF stays
                    // zero, since guest RAM is zero-initialized).
                    if let Some(file) = self.fs.open_files.get(&fd) {
                        let mut scratch = vec![0u8; len as usize];
                        if let Ok(n) = file.read_at(&mut scratch, off) {
                            vm.write_bytes(target, &scratch[..n]).expect("mmap target mapped");
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
            SYS_STAT => {
                let path = read_cstr(vm, cpu.reg(Reg::Rdi));
                let size = self
                    .fs
                    .allow
                    .iter()
                    .find(|p| p.as_os_str().as_encoded_bytes() == path)
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len());
                let ret = match size {
                    Some(sz) => {
                        write_stat(vm, cpu.reg(Reg::Rsi), sz);
                        0
                    }
                    None => (-2i64) as u64, // -ENOENT
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_FSTAT => {
                let fd = cpu.reg(Reg::Rdi);
                let size = self.fs.open_files.get(&fd).and_then(|f| f.metadata().ok()).map(|m| m.len());
                let ret = match size {
                    Some(sz) => {
                        write_stat(vm, cpu.reg(Reg::Rsi), sz);
                        0
                    }
                    None => (-9i64) as u64, // -EBADF
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_PREAD64 => {
                let fd = cpu.reg(Reg::Rdi);
                let buf = cpu.reg(Reg::Rsi);
                let len = cpu.reg(Reg::Rdx) as usize;
                let off = cpu.reg(Reg::R10);
                let ret = match self.fs.open_files.get(&fd) {
                    Some(file) => {
                        let mut scratch = vec![0u8; len];
                        match file.read_at(&mut scratch, off) {
                            Ok(n) => {
                                vm.write_bytes(buf, &scratch[..n]).expect("pread buffer mapped");
                                n as u64
                            }
                            Err(_) => EBADF,
                        }
                    }
                    None => EBADF,
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_NEWFSTATAT => {
                // fstatat(dirfd, path, statbuf, flags). Only allowlisted paths exist.
                let path = read_cstr(vm, cpu.reg(Reg::Rsi));
                let size = self
                    .fs
                    .allow
                    .iter()
                    .find(|p| p.as_os_str().as_encoded_bytes() == path)
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len());
                let ret = match size {
                    Some(sz) => {
                        write_stat(vm, cpu.reg(Reg::Rdx), sz);
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
            SYS_LSEEK => {
                // Seek a passthrough file; unknown fd → -EBADF.
                let fd = cpu.reg(Reg::Rdi);
                let off = cpu.reg(Reg::Rsi) as i64;
                let whence = cpu.reg(Reg::Rdx);
                let ret = match self.fs.open_files.get_mut(&fd) {
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
                };
                cpu.set_reg(Reg::Rax, ret);
                false
            }
            SYS_ACCESS => {
                cpu.set_reg(Reg::Rax, (-2i64) as u64); // -ENOENT: nothing exists in the harness
                false
            }
            SYS_FCNTL => {
                // F_SETFD/F_SETLK/F_GETFL etc. — benign: succeed / report O_RDONLY.
                cpu.set_reg(Reg::Rax, 0);
                false
            }
            SYS_GETPID => {
                cpu.set_reg(Reg::Rax, 1000);
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
            SYS_EXIT | SYS_EXIT_GROUP => {
                self.exit_code = Some(cpu.reg(Reg::Rdi) as i32);
                true
            }
            other => {
                let ret = self
                    .scripted
                    .get(other)
                    .unwrap_or_else(|| panic!("unhandled syscall {other}"));
                cpu.set_reg(Reg::Rax, ret);
                false
            }
        }
    }

    /// Resolve a guest `open`: read the C-string path from guest memory, check it
    /// against the allowlist, and host-open read-only. Returns a guest fd or a
    /// negative errno.
    fn do_open(&mut self, vm: &Vm, path_ptr: u64, flags: u64) -> u64 {
        if (flags & O_ACCMODE) != O_RDONLY {
            return EACCES; // writes never pass through
        }
        let path = read_cstr(vm, path_ptr);
        // Resolve to a host file: an exact allowlist entry, or a suffix redirect
        // (but never a `glibc-hwcaps` probe — those must fail like native).
        let host: Option<PathBuf> = if self
            .fs
            .allow
            .iter()
            .any(|p| p.as_os_str().as_encoded_bytes() == path)
        {
            Some(PathBuf::from(String::from_utf8_lossy(&path).into_owned()))
        } else if !contains(&path, b"glibc-hwcaps") {
            self.fs
                .serve
                .iter()
                .find(|(suffix, _)| path.ends_with(suffix.as_slice()))
                .map(|(_, host)| host.clone())
        } else {
            None
        };
        // Not resolvable → "no such file" (a dynamic loader probes many paths).
        let Some(host) = host else { return ENOENT };
        match File::open(host) {
            Ok(f) => {
                let fd = self.fs.next_fd;
                self.fs.next_fd += 1;
                self.fs.open_files.insert(fd, f);
                fd
            }
            Err(_) => ENOENT,
        }
    }

    /// Resolve a guest `read`: pull bytes from the host file into a scratch buffer,
    /// then copy them into guest memory. Returns the byte count or a negative errno.
    fn do_read(&mut self, vm: &mut Vm, fd: u64, buf: u64, len: usize) -> u64 {
        let Some(file) = self.fs.open_files.get_mut(&fd) else {
            return EBADF;
        };
        let mut scratch = vec![0u8; len];
        match file.read(&mut scratch) {
            Ok(n) => {
                vm.write_bytes(buf, &scratch[..n]).expect("read buffer is mapped");
                n as u64
            }
            Err(_) => EBADF,
        }
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

/// Write a minimal x86-64 `struct stat` (144 bytes) describing a regular file of
/// `size` bytes: enough for the size/mode checks a hashing utility makes.
fn write_stat(vm: &mut Vm, addr: u64, size: u64) {
    let mut buf = [0u8; 144];
    buf[16..24].copy_from_slice(&1u64.to_le_bytes()); // st_nlink = 1
    buf[24..28].copy_from_slice(&0o100644u32.to_le_bytes()); // st_mode = S_IFREG|0644
    buf[48..56].copy_from_slice(&size.to_le_bytes()); // st_size
    buf[56..64].copy_from_slice(&512u64.to_le_bytes()); // st_blksize
    buf[64..72].copy_from_slice(&size.div_ceil(512).to_le_bytes()); // st_blocks
    let _ = vm.write_bytes(addr, &buf);
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
