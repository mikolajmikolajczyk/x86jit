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
//! Ubuntu is ~45 MiB and a moving target, so instead of a git-ignored `docker save`
//! tar (which no-op'd in CI) it is **pulled from the registry**, digest-pinned, via
//! the shared `pull_image` helper (decision-10). When `skopeo` is absent or there is
//! no network egress the test no-ops with a note.

mod common;
use common::{oci_archive, pull_image, skopeo_present};

// ubuntu, pinned by its amd64 manifest digest (public.ecr.aws — Docker Hub mirror,
// no anon rate limit). Bump the digest to move to a newer release.
const IMAGE: &str = "public.ecr.aws/docker/library/ubuntu";
const DIGEST: &str = "sha256:c6c0067e0e45b7a826eaebb193cef957be28045380963a9b1eeb2a5d3c70a1b9";

#[test]
fn ubuntu_dash_echo_runs_three_ways() {
    if !skopeo_present() {
        eprintln!("skipping: skopeo not on PATH (registry pull needs it; see decision-10)");
        return;
    }
    let Some(tar) = pull_image(IMAGE, DIGEST) else {
        eprintln!("skipping: could not pull {IMAGE}@{DIGEST} (no network egress?)");
        return;
    };
    oci_archive(tar, "ubuntu-dash")
        .argv(&["/usr/bin/dash", "-c", "echo hello from ubuntu on x86jit"])
        .expect_stdout(b"hello from ubuntu on x86jit\n")
        .expect_exit(0)
        .run();
}
