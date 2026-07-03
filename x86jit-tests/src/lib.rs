//! Test harness for x86jit (§13).
//!
//! Strategy (in priority order):
//! 1. Differential testing — run a block natively on an x86 host and compare
//!    register/flag state against the interpreter (the free oracle).
//! 2. Interpreter as oracle for the JIT — identical state on every block.
//! 3. Per-instruction unit tests, including edge cases (overflow, zero, sign).
//! 4. Decoder fuzzing — random bytes must never panic the lift.
//! 5. A growing corpus of real static ELF binaries as end-to-end tests.

/// Compare two `CpuState`s field by field; returns the first mismatch.
pub fn diff_state() {
    // Placeholder — implemented alongside M1 differential tests.
}
