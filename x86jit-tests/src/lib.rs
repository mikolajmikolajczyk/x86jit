//! Test harness for x86jit (testing.md). The reusable spine for M1–M5:
//! self-contained RON [`vector`]s, an [`oracle`] abstraction (interpreter under
//! test + Unicorn truth), and a precise [`compare`]ator with undefined-flag
//! masking.
//!
//! The Unicorn oracle and `capture` CLI are gated behind the `unicorn` feature so
//! the core harness builds without the native Unicorn library.

pub mod builder;
pub mod compare;
pub mod compat;
pub mod fuzz;
pub mod oracle;
pub mod reference;
pub mod syscall;
pub mod vector;

#[cfg(feature = "unicorn")]
pub mod unicorn;
