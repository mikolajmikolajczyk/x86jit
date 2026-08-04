//! Test harness for x86jit (testing.md). The reusable spine for M1–M5:
//! self-contained RON [`vector`]s, an [`oracle`] abstraction (interpreter under
//! test + Unicorn truth), and a precise [`compare`]ator with undefined-flag
//! masking.
//!
//! The Unicorn oracle and `capture` CLI are gated behind the `unicorn` feature so
//! the core harness builds without the native Unicorn library.
//!
//! **Scope.** This harness validates the *recompiler*: instruction semantics, the
//! JIT against the interpreter, the fuzzers, the ISA coverage map. The whole-program
//! ladder — busybox, sqlite, CPython, the Go servers — validates the recompiler *plus*
//! a Linux userland, so it lives with that userland in `unemulinux`, along with the
//! `Guest` builder and the fetched fixtures it needs.
//!
//! **One exception, and it is deliberate.** `tests/mt.rs` loads a static-musl pthreads
//! binary and services its handful of syscalls (`write`, `futex`, `clone`, `mmap`,
//! `arch_prctl`, exit) *inline* in the test. It is not an embedder: there is no shim
//! crate, no process model, no filesystem — just enough to let four guest threads reach
//! a futex. That is the only way to exercise the M7 threading substrate (one `Arc<Vm>`,
//! a host thread per guest thread, real cross-thread atomics) against a genuine program,
//! which is engine behaviour, not OS behaviour. It is why `x86jit-elf` and the
//! `reference` helper are still dependencies here. If that inline shim ever grows past
//! those syscalls, the test has stopped being an engine test and belongs downstream.

pub mod builder;
pub mod compare;
pub mod compat;
pub mod fuzz;
// NativeOracle (testing.md §4): execute the guest snippet on the real host CPU.
// x86-64/Linux only — the fastest independent oracle on the desktop, and the only
// one that can oracle VEX/EVEX ops Unicorn's QEMU build can't decode (task-130).
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
pub mod native;
pub mod oracle;
// Native-vs-baked-expectation helper. Generic (no OS, no shim), so it stays here and
// `unemulinux` reuses it through this crate rather than keeping a second copy.
pub mod reference;
// SingleStepTests 80286 corpus loader — the authoritative Real16 oracle (our target CPU).
pub mod ss286;
pub mod vector;

#[cfg(feature = "unicorn")]
pub mod unicorn;
