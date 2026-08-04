//! Test harness for x86jit (testing.md). The reusable spine for M1–M5:
//! self-contained RON [`vector`]s, an [`oracle`] abstraction (interpreter under
//! test + Unicorn truth), and a precise [`compare`]ator with undefined-flag
//! masking.
//!
//! The Unicorn oracle and `capture` CLI are gated behind the `unicorn` feature so
//! the core harness builds without the native Unicorn library.
//!
//! **Scope.** This harness validates the *recompiler*: instruction semantics, the
//! JIT against the interpreter, the fuzzers, the ISA coverage map. It needs no
//! operating system and no guest fixtures. The whole-program ladder — busybox,
//! sqlite, CPython, the Go servers — validates the recompiler *plus* a Linux
//! userland, so it lives with that userland in `unemulinux` along with the
//! `Guest` builder, the `reference` oracle and the fetched fixtures they need.

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
