//! Test harness for x86jit (testing.md). The reusable spine for M1–M5:
//! self-contained RON [`vector`]s, an [`oracle`] abstraction (interpreter under
//! test + Unicorn truth), and a precise [`compare`]ator with undefined-flag
//! masking.
//!
//! The Unicorn oracle and `capture` CLI are gated behind the `unicorn` feature so
//! the core harness builds without the native Unicorn library.

/// The 32-byte raw digest `programs/sha256.elf` emits for its fixed input. Shared
/// by the whole-program test and the bench workload so regenerating the fixture is
/// a single edit (#17).
pub const SHA256_FIXTURE_DIGEST: &[u8] = b"\xe7\x2b\x9a\x3d\x7e\x6f\x05\x3e\x6b\xbd\x38\x8c\xa2\x8b\x15\x49\
\xf0\x21\x25\xf7\x62\x94\x4a\x9b\x81\x11\x96\x97\xdd\xd1\x7d\x94";

pub mod builder;
pub mod compare;
pub mod compat;
pub mod guest;
pub mod fuzz;
pub mod oracle;
pub mod reference;
// The syscall shim graduated to the x86jit-linux embedder crate (OCI-1);
// re-exported here so the existing test suite's `x86jit_tests::syscall` paths keep
// working unchanged.
pub use x86jit_linux::shim as syscall;
pub mod vector;

#[cfg(feature = "unicorn")]
pub mod unicorn;
