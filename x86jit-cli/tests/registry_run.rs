//! `x86jit-cli oci run` end-to-end (task-183 P1/P2): pull a **digest-pinned** image
//! straight from a registry with the built-in OCI-distribution client (no skopeo, no
//! Docker daemon, no committed tar), then run a command in it through the recompiler.
//!
//! The image comes from **public.ecr.aws** (AWS's Docker Hub mirror — no anon rate
//! limit) and is pinned by its amd64 manifest digest for reproducibility. When there's
//! no network egress (e.g. a fork's CI), the test no-ops with a note instead of
//! failing — the same policy as the skopeo-based `registry_pull` test.

use std::path::PathBuf;

use x86jit_cli::{run_registry, EngineKind, RunOptions};

// busybox (glibc), pinned by its amd64 manifest digest — never a moving `:latest`.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox";
const DIGEST: &str = "sha256:1cfa4e2b09e127b9c4ed43578d3f3c18e7d44ea47b9ea98475c0cbe9086525f8";

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("x86jit-cli-registry-run-{tag}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn registry_run_pulls_and_runs_busybox() {
    let reference = format!("{IMAGE}@{DIGEST}");
    let argv: Vec<String> = ["/bin/busybox", "echo", "pulled-and-ran"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    // Blob cache: honor an inherited `X86JIT_OCI_CACHE` (CI persists it via
    // `actions/cache`), else a temp dir for the local mechanism check. Content-addressed
    // by digest, so it's additive — no need to wipe. The `interp` pull warms it; the
    // `jit` pull is then served from cache, no second registry hit.
    let cache = std::env::var_os("X86JIT_OCI_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("x86jit-cli-registry-run-cache"));
    std::env::set_var("X86JIT_OCI_CACHE", &cache);

    // Run on both engines; interp == jit is the invariant, jit reuses interp's cache.
    for (tag, engine) in [
        ("interp", EngineKind::Interpreter),
        ("jit", EngineKind::Jit),
    ] {
        let rootfs = scratch(tag);
        match run_registry(
            &reference,
            &rootfs,
            engine,
            &argv,
            RunOptions::default(),
            false,
        ) {
            Ok(res) => {
                assert_eq!(res.stdout, b"pulled-and-ran\n", "{tag} stdout");
                assert_eq!(res.exit_code, Some(0), "{tag} exit");
            }
            Err(e) => {
                // No network egress (or the mirror is down): skip, don't fail.
                eprintln!("skipping: could not pull {reference} on {tag} ({e})");
                return;
            }
        }
    }

    // The pulls populated the content-addressed cache (manifest + config + layers).
    let cached = std::fs::read_dir(&cache).map(|d| d.count()).unwrap_or(0);
    assert!(
        cached >= 2,
        "expected the blob cache to be populated, found {cached} files"
    );
}
