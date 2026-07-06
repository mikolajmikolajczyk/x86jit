//! A real, modern **ubuntu:latest** OCI image runs a shell command three ways
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
//! The `ubuntu.tar` fixture is large and a moving target, so it is git-ignored and
//! regenerated locally (see `x86jit-oci/fixtures/README.md`). When it is absent
//! this test no-ops with a note instead of failing.

mod common;
use common::oci;
use std::path::Path;

/// The git-ignored fixture; absent on a fresh checkout / CI without a local pull.
fn fixture_present() -> bool {
    let p = format!(
        "{}/../x86jit-oci/fixtures/ubuntu.tar",
        env!("CARGO_MANIFEST_DIR")
    );
    Path::new(&p).exists()
}

#[test]
fn ubuntu_dash_echo_runs_three_ways() {
    if !fixture_present() {
        eprintln!(
            "skipping: x86jit-oci/fixtures/ubuntu.tar not present \
             (regenerate: docker pull ubuntu:latest && docker save ubuntu:latest \
             -o x86jit-oci/fixtures/ubuntu.tar)"
        );
        return;
    }
    oci("ubuntu.tar", "ubuntu-dash")
        .argv(&["/usr/bin/dash", "-c", "echo hello from ubuntu on x86jit"])
        .expect_stdout(b"hello from ubuntu on x86jit\n")
        .expect_exit(0)
        .run();
}
