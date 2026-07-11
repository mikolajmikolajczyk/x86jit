//! Linux x86-64 userland embedder for x86jit (spec §1/§4.1).
//!
//! `x86jit-core` executes guest instructions and traps out on `Exit::Syscall`; this
//! crate is the *embedder* that services those traps — the Linux syscall shim
//! ([`shim::LinuxShim`]), and (as the OCI track climbs) the guest filesystem and the
//! multi-process model. None of this belongs in the core: file formats, OS syscalls,
//! and devices live here, on the embedder side of the boundary.
//!
//! Graduated out of `x86jit-tests` (where it began as test-harness code) so it can
//! back a real image runner, not just the differential suite.

pub mod hostmem;
pub mod proc;
pub mod shim;
pub mod sigsegv;
pub mod thread;

pub use proc::{ExecImage, ProcError, ProcOutcome, Scheduler};
pub use shim::{EntropyMode, LinuxShim};
pub use thread::{run_threaded, ThreadShared};

/// Log a guest trap loudly the instant it happens, so an unsupported instruction
/// surfaces on stderr immediately — not only via a returned error a caller might
/// swallow (the `Exit::UnknownInstruction` analogue of the shim's `gap:syscall` log).
/// `UnknownInstruction` prints the exact opcode bytes so the missing instruction is
/// decodable at a glance: task-132 took hours to pin *precisely* because a threaded
/// worker's join hid this trap instead of it screaming. It's terminal on these paths
/// (the loop returns), so one line, no dedup needed.
pub(crate) fn report_gap(exit: &x86jit_core::Exit) {
    if let x86jit_core::Exit::UnknownInstruction { addr, bytes, len } = exit {
        let hex: Vec<String> = bytes[..*len as usize]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        eprintln!(
            "x86jit: UNKNOWN INSTRUCTION at {addr:#x}: {} (gap:instruction)",
            hex.join(" ")
        );
    }
}
