//! A real, modern **ubuntu** OCI image runs a shell command three ways
//! (interp == JIT). Ubuntu is glibc-dynamic like busybox:glibc, but a current
//! release exercises paths the smaller images don't:
//!
//! - the dynamic loader maps a big image clear of the interpreter (ubuntu's
//!   ~11 MiB uutils `coreutils` PIE would collide a fixed low interp base — the
//!   loader now derives the interp base above the exe span);
//! - modern glibc selects SSSE3 string routines (`pshufb`/`palignr`) once
//!   SSE4.1/4.2 are un-advertised (backlog/decisions/decision-2 - cpuid-drop-sse4.md);
//! - `dash` startup needs `poll`/`statfs`/`prctl`, added to the shim.
//!
//! Ubuntu is ~45 MiB and a moving target, so it is **pulled digest-pinned from the
//! registry** with the built-in client (no committed tar, no skopeo). When there is
//! no network egress the test no-ops with a note.

mod common;
use common::{oci, UBUNTU};

#[test]
fn ubuntu_dash_echo_runs_three_ways() {
    oci(UBUNTU, "ubuntu-dash")
        .argv(&["/usr/bin/dash", "-c", "echo hello from ubuntu on x86jit"])
        .expect_stdout(b"hello from ubuntu on x86jit\n")
        .expect_exit(0)
        .run();
}
