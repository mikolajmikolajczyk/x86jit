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

pub mod shim;

pub use shim::LinuxShim;
