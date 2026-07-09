//! Shared harness for the OCI image tests. Every case is the same shape — pull a
//! **digest-pinned** image straight from a registry into a fresh rootfs with the
//! built-in client (no `skopeo`, no committed tar), run the entrypoint (or an argv
//! override) under the interpreter and the JIT, and assert they agree — so it lives
//! here once instead of being re-typed per file. The invariant `interp == jit` (and,
//! when a native oracle is available, `== native`) is enforced by [`Case::run`];
//! anything case-specific (a digest, a `starts_with`, a file compare) reads off the
//! returned [`Ran`].
//!
//! Images come from **public.ecr.aws** (AWS's Docker Hub mirror — no anon rate limit),
//! pinned by digest for reproducibility. Blobs are cached content-addressed (see
//! `X86JIT_OCI_CACHE`), so a registry is hit at most once per digest. When there's no
//! network egress (e.g. a fork's CI), [`Case::run`] no-ops with a note instead of
//! failing.
//!
//! Not itself a test binary (a `tests/common/` submodule); each test file does
//! `mod common;`. `allow(dead_code)` because no single test binary uses every helper.
#![allow(dead_code)]

use std::path::PathBuf;

use x86jit_cli::{registry, run_config_argv_stdin, EngineKind, RunResult};

// Digest-pinned images (public.ecr.aws — no anon rate limit). Resolved once; immutable.
pub const BUSYBOX_MUSL: &str = "public.ecr.aws/docker/library/busybox@sha256:8635836765b0c4c43970660219739baa58b0883c2e429e4b8918f7dd1519455c";
pub const BUSYBOX_GLIBC: &str = "public.ecr.aws/docker/library/busybox@sha256:1cfa4e2b09e127b9c4ed43578d3f3c18e7d44ea47b9ea98475c0cbe9086525f8";
pub const ALPINE: &str = "public.ecr.aws/docker/library/alpine@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b";
pub const HELLO_WORLD: &str = "public.ecr.aws/docker/library/hello-world@sha256:96498ffd522e70807ab6384a5c0485a79b9c7c08ca79ba08623edcad1054e62d";
pub const UBUNTU: &str = "public.ecr.aws/docker/library/ubuntu@sha256:c6c0067e0e45b7a826eaebb193cef957be28045380963a9b1eeb2a5d3c70a1b9";

/// How to obtain the native (host) oracle for the three-way comparison.
pub enum Native {
    /// No native leg — the guest binary can't be trusted to run on the host (a
    /// musl-dynamic image on a glibc host) or resolves paths inside the rootfs the
    /// host can't see (`execve`/pipeline via a rootfs-relative command). `interp ==
    /// jit` against a known/derived expected value is the oracle instead.
    Skip,
    /// Run `argv[0]` (resolved inside the rootfs) on the host with `argv[1..]`, and
    /// require `interp == jit == native`.
    Host,
}

/// A configured OCI test case. Build it with [`oci`], then [`Case::run`].
pub struct Case {
    /// Digest-pinned image reference to pull.
    reference: &'static str,
    /// Names the case's (unique) scratch rootfs.
    tag: &'static str,
    argv: Vec<String>,
    files: Vec<(String, Vec<u8>)>,
    stdin: Vec<u8>,
    native: Native,
    expect_stdout: Option<Vec<u8>>,
    expect_exit: Option<i32>,
}

/// Start a case: pull `reference` (a digest-pinned image) into the scratch rootfs
/// named by `tag`.
pub fn oci(reference: &'static str, tag: &'static str) -> Case {
    Case {
        reference,
        tag,
        argv: Vec::new(),
        files: Vec::new(),
        stdin: Vec::new(),
        native: Native::Skip,
        expect_stdout: None,
        expect_exit: None,
    }
}

impl Case {
    /// Override the entrypoint: run this argv instead of the image's default
    /// `Entrypoint`+`Cmd`. `argv[0]` is the program path (resolved in the rootfs).
    pub fn argv(mut self, argv: &[&str]) -> Self {
        self.argv = argv.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Shorthand for a `busybox sh -c <script>` entrypoint.
    pub fn sh(self, script: &str) -> Self {
        self.argv(&["/bin/busybox", "sh", "-c", script])
    }

    /// Write `bytes` to `rel` inside the rootfs before running (a file the guest
    /// reads through GuestFs).
    pub fn file(mut self, rel: &str, bytes: &[u8]) -> Self {
        self.files.push((rel.to_string(), bytes.to_vec()));
        self
    }

    pub fn native(mut self, native: Native) -> Self {
        self.native = native;
        self
    }

    /// Seed the guest's stdin (fd 0) — e.g. an HTTP request fed to `httpd -i`.
    pub fn stdin(mut self, bytes: &[u8]) -> Self {
        self.stdin = bytes.to_vec();
        self
    }

    /// Assert the (interp == jit) stdout equals `bytes`.
    pub fn expect_stdout(mut self, bytes: &[u8]) -> Self {
        self.expect_stdout = Some(bytes.to_vec());
        self
    }

    /// Assert both engines exit with `code`.
    pub fn expect_exit(mut self, code: i32) -> Self {
        self.expect_exit = Some(code);
        self
    }

    /// Pull the image, run it both ways (plus native if requested), enforce the
    /// agreement invariants and any `expect_*`, and return the results for
    /// case-specific assertions. `None` when the pull fails (no network egress) — the
    /// caller then no-ops; a test that inspects the result does `let Some(ran) = … else
    /// { return }`, one that only set `expect_*` can discard it.
    pub fn run(self) -> Option<Ran> {
        let rootfs = std::env::temp_dir().join(format!("x86jit-oci-{}", self.tag));
        let _ = std::fs::remove_dir_all(&rootfs);
        std::fs::create_dir_all(&rootfs).unwrap();

        let cfg = match registry::pull(self.reference, &rootfs, false) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("skipping {}: pull failed ({e})", self.tag);
                return None;
            }
        };
        for (rel, bytes) in &self.files {
            std::fs::write(rootfs.join(rel), bytes).unwrap();
        }

        // The effective argv: an explicit override, else the image's own entrypoint.
        let argv: Vec<String> = if self.argv.is_empty() {
            cfg.argv()
        } else {
            self.argv.clone()
        };

        let native = match self.native {
            Native::Skip => None,
            // The guest is an x86-64 ELF, so only exec it natively on an x86-64 host
            // (mirrors x86jit-tests `reference`); on the ARM CI runner the native leg
            // is skipped and interp == jit carries the validation.
            #[cfg(target_arch = "x86_64")]
            Native::Host => {
                let bin = rootfs.join(argv[0].trim_start_matches('/'));
                let out = std::process::Command::new(&bin)
                    .args(&argv[1..])
                    .output()
                    .unwrap_or_else(|e| panic!("native {}: {e}", bin.display()));
                Some(out.stdout)
            }
            #[cfg(not(target_arch = "x86_64"))]
            Native::Host => None,
        };

        let interp =
            run_config_argv_stdin(&cfg, &rootfs, EngineKind::Interpreter, &argv, &self.stdin)
                .expect("interpreter run");
        let jit = run_config_argv_stdin(&cfg, &rootfs, EngineKind::Jit, &argv, &self.stdin)
            .expect("jit run");

        assert_eq!(interp.stdout, jit.stdout, "interp == jit stdout");
        assert_eq!(interp.exit_code, jit.exit_code, "interp == jit exit code");
        if let Some(n) = &native {
            assert_eq!(&interp.stdout, n, "interp == native");
            assert_eq!(&jit.stdout, n, "jit == native");
        }
        if let Some(exp) = &self.expect_stdout {
            assert_eq!(&interp.stdout, exp, "stdout == expected");
        }
        if let Some(code) = self.expect_exit {
            assert_eq!(interp.exit_code, Some(code), "exit == expected");
            assert_eq!(jit.exit_code, Some(code));
        }

        Some(Ran {
            interp,
            jit,
            native,
            rootfs,
        })
    }
}

/// The result of a [`Case::run`], for case-specific assertions beyond the invariants.
pub struct Ran {
    pub interp: RunResult,
    pub jit: RunResult,
    pub native: Option<Vec<u8>>,
    pub rootfs: PathBuf,
}

impl Ran {
    /// The stdout the engines produced (`interp == jit` is already asserted).
    pub fn stdout(&self) -> &[u8] {
        &self.interp.stdout
    }

    /// The first whitespace-delimited token of stdout — a checksum tool's digest.
    pub fn first_token(&self) -> String {
        String::from_utf8_lossy(self.stdout())
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string()
    }
}
