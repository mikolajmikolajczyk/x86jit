//! OCI-3: the real alpine image (a musl-dynamic busybox) runs `cat /etc/os-release`
//! three ways. Exercises the ld-musl dynamic loader path and graceful -ENOSYS for
//! syscalls busybox probes then falls back from (sendfile -> read/write).

use std::path::{Path, PathBuf};
use x86jit_oci::load_image;
use x86jit_run::{run_config_argv, EngineKind};

const IMAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../x86jit-oci/fixtures/alpine.tar");

fn scratch(n: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("x86jit-alpine-{n}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn alpine_cat_os_release_interp_eq_jit() {
    let rootfs = scratch("cat");
    let cfg = load_image(Path::new(IMAGE), &rootfs).expect("load alpine");
    // Expected output = the file's own content (native musl busybox may not run on
    // a glibc host, so the rootfs file is the oracle).
    let expected = std::fs::read(rootfs.join("etc/os-release")).expect("os-release");

    let argv: Vec<String> = ["/bin/busybox", "cat", "/etc/os-release"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let interp = run_config_argv(&cfg, &rootfs, EngineKind::Interpreter, &argv).unwrap();
    let jit = run_config_argv(&cfg, &rootfs, EngineKind::Jit, &argv).unwrap();

    assert_eq!(interp.stdout, jit.stdout, "interp == jit");
    assert_eq!(interp.stdout, expected, "cat output == the file");
    assert!(interp.stdout.starts_with(b"NAME=\"Alpine Linux\""));
    assert_eq!((interp.exit_code, jit.exit_code), (Some(0), Some(0)));
}
