//! decision-10 / task-167: pull a **digest-pinned** OCI image straight from a
//! registry (no Docker daemon, no committed tar, no hand-built ELF) and run a
//! swapped-entrypoint command under interp == jit.
//!
//! `skopeo copy … docker-archive:` writes a `docker save`-format tar, which
//! `x86jit-oci::load_image` already reads — so the pull is the only new step; all
//! rootfs/config/run machinery is reused.
//!
//! The image is pulled from **public.ecr.aws** (AWS's Docker Hub mirror — no anon
//! rate limit) and pinned by digest for reproducibility. The tar is cached under
//! `target/oci-pull-cache/<digest>.tar`, so the registry is hit at most once per
//! digest. When `skopeo` is absent or the pull fails (no network egress, e.g. a
//! fork's CI), the test no-ops with a note instead of failing — the same policy as
//! the git-ignored `ubuntu.tar` fixture.

mod common;
use common::{oci_archive, pull_image, skopeo_present, Native};

// busybox (glibc), pinned by its amd64 manifest digest — never a moving `:latest`.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox";
const DIGEST: &str = "sha256:1cfa4e2b09e127b9c4ed43578d3f3c18e7d44ea47b9ea98475c0cbe9086525f8";

#[test]
fn registry_pulled_busybox_runs_three_ways() {
    if !skopeo_present() {
        eprintln!("skipping: skopeo not on PATH (registry pull needs it; see decision-10)");
        return;
    }
    let Some(tar) = pull_image(IMAGE, DIGEST) else {
        eprintln!("skipping: could not pull {IMAGE}@{DIGEST} (no network egress?)");
        return;
    };
    // Swap the entrypoint: run a command in the pulled rootfs. `Native::Host` adds the
    // host oracle on x86-64 (skipped on the ARM runner, where interp == jit carries).
    oci_archive(tar, "busybox-registry")
        .argv(&["/bin/busybox", "echo", "hello-from-a-pulled-image"])
        .native(Native::Host)
        .expect_stdout(b"hello-from-a-pulled-image\n")
        .expect_exit(0)
        .run();
}
