//! Test-side Linux x86-64 syscall shim (testing.md §9). The core never emulates
//! an OS (§1); this is a thin embedder that reacts to `Exit::Syscall`.
//!
//! Convention: number in RAX, args in RDI/RSI/RDX/R10/R8/R9, return in RAX. RIP
//! already points past the `syscall` (the engine's convention), so the driver
//! just calls `run()` again to resume.

use x86jit_core::{Reg, Vcpu, Vm};

const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 60;
const SYS_EXIT_GROUP: u64 = 231;

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

/// Captures a program's observable output: bytes written to stdout/stderr and the
/// exit code. Compare these (not raw state) for a deterministic whole-program
/// oracle (testing.md §12.3).
#[derive(Default)]
pub struct LinuxShim {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub scripted: ScriptedSyscalls,
}

impl LinuxShim {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle one `Exit::Syscall`. Returns `true` when the program has exited.
    pub fn handle(&mut self, cpu: &mut Vcpu, vm: &Vm) -> bool {
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
}
