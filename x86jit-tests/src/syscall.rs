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
use std::path::PathBuf;

use x86jit_core::{Reg, Vcpu, Vm};

const SYS_READ: u64 = 0;
const SYS_WRITE: u64 = 1;
const SYS_OPEN: u64 = 2;
const SYS_CLOSE: u64 = 3;
const SYS_BRK: u64 = 12;
const SYS_ARCH_PRCTL: u64 = 158;
const SYS_SET_TID_ADDRESS: u64 = 218;
const SYS_EXIT: u64 = 60;
const SYS_OPENAT: u64 = 257;
const SYS_EXIT_GROUP: u64 = 231;
const ARCH_SET_FS: u64 = 0x1002;

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
    open_files: HashMap<u64, File>,
    next_fd: u64,
}

impl FsPassthrough {
    fn enabled(&self) -> bool {
        !self.allow.is_empty()
    }
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
    fs: FsPassthrough,
}

impl LinuxShim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Permit read-only host passthrough for exactly the given paths (testing.md
    /// §12). Any `open` of a path not on the list returns `-EACCES`.
    pub fn allow_read(&mut self, path: impl Into<PathBuf>) {
        self.fs.next_fd = self.fs.next_fd.max(3);
        self.fs.allow.push(path.into());
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
        if !self.fs.enabled() || (flags & O_ACCMODE) != O_RDONLY {
            return EACCES;
        }
        let path = read_cstr(vm, path_ptr);
        let allowed = self.fs.allow.iter().any(|p| p.as_os_str().as_encoded_bytes() == path);
        if !allowed {
            return EACCES;
        }
        match File::open(PathBuf::from(String::from_utf8_lossy(&path).into_owned())) {
            Ok(f) => {
                let fd = self.fs.next_fd;
                self.fs.next_fd += 1;
                self.fs.open_files.insert(fd, f);
                fd
            }
            Err(_) => EACCES,
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
