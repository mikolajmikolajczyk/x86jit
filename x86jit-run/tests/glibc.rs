//! OCI-3: the real busybox:glibc image (a dynamic glibc PIE) runs applets three
//! ways (native == interpreter == JIT). Exercises the dynamic loader path
//! (ld-linux + libc.so loaded from the rootfs via GuestFs) and fxsave/fxrstor,
//! which glibc's PLT resolver uses to preserve XMM across symbol resolution.

use std::path::{Path, PathBuf};

use x86jit_oci::{load_image, ImageConfig};
use x86jit_run::{run_config_argv, EngineKind, RunResult};

const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-oci/fixtures/busybox-glibc.tar"
);

fn setup(name: &str) -> (ImageConfig, PathBuf) {
    let rootfs = std::env::temp_dir().join(format!("x86jit-gl-{name}"));
    let _ = std::fs::remove_dir_all(&rootfs);
    std::fs::create_dir_all(&rootfs).unwrap();
    let cfg = load_image(Path::new(IMAGE), &rootfs).expect("load busybox:glibc");
    (cfg, rootfs)
}

fn run(cfg: &ImageConfig, rootfs: &Path, e: EngineKind, a: &[&str]) -> RunResult {
    let argv: Vec<String> = a.iter().map(|s| s.to_string()).collect();
    run_config_argv(cfg, rootfs, e, &argv).expect("run applet")
}

#[test]
fn busybox_glibc_echo_runs_three_ways() {
    let (cfg, rootfs) = setup("echo");
    let a = ["/bin/busybox", "echo", "dynamic glibc on x86jit"];
    let native = std::process::Command::new(rootfs.join("bin/busybox"))
        .args(&a[1..])
        .output()
        .expect("native busybox:glibc");
    assert_eq!(native.stdout, b"dynamic glibc on x86jit\n");

    let interp = run(&cfg, &rootfs, EngineKind::Interpreter, &a);
    let jit = run(&cfg, &rootfs, EngineKind::Jit, &a);
    assert_eq!(interp.stdout, native.stdout, "interp == native");
    assert_eq!(jit.stdout, native.stdout, "jit == native");
    assert_eq!((interp.exit_code, jit.exit_code), (Some(0), Some(0)));
}

#[test]
fn busybox_glibc_sha256sum_reads_rootfs() {
    let (cfg, rootfs) = setup("sha");
    std::fs::write(rootfs.join("data.txt"), b"hello world\n").unwrap();
    let a = ["/bin/busybox", "sha256sum", "/data.txt"];
    const DIGEST: &str = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";

    let interp = run(&cfg, &rootfs, EngineKind::Interpreter, &a);
    let jit = run(&cfg, &rootfs, EngineKind::Jit, &a);
    assert_eq!(interp.stdout, jit.stdout, "interp == jit");
    let out = String::from_utf8_lossy(&interp.stdout);
    assert_eq!(out.split_whitespace().next().unwrap_or(""), DIGEST);
}
