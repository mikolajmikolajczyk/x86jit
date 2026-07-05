//! OCI-2 acceptance: the real `busybox:musl` image (a static-PIE single binary)
//! runs applets directly — no `sh -c`, no fork — three ways (native == interpreter
//! == JIT). Exercises static-PIE loading, the rootfs guest filesystem, and the
//! static-musl syscall surface.

use std::path::{Path, PathBuf};

use x86jit_oci::{load_image, ImageConfig};
use x86jit_run::{run_config_argv, EngineKind, RunResult};

const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-oci/fixtures/busybox-musl.tar"
);

fn setup(name: &str) -> (ImageConfig, PathBuf) {
    let rootfs = std::env::temp_dir().join(format!("x86jit-bb-{name}"));
    let _ = std::fs::remove_dir_all(&rootfs);
    std::fs::create_dir_all(&rootfs).unwrap();
    let cfg = load_image(Path::new(IMAGE), &rootfs).expect("load busybox image");
    (cfg, rootfs)
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn run(cfg: &ImageConfig, rootfs: &Path, engine: EngineKind, a: &[&str]) -> RunResult {
    run_config_argv(cfg, rootfs, engine, &argv(a)).expect("run applet")
}

/// `busybox echo` — plain applet, three ways, full stdout must match.
#[test]
fn busybox_echo_runs_three_ways() {
    let (cfg, rootfs) = setup("echo");
    let a = ["/bin/busybox", "echo", "hello from x86jit"];

    let native = std::process::Command::new(rootfs.join("bin/busybox"))
        .args(&a[1..])
        .output()
        .expect("native busybox");
    assert_eq!(native.stdout, b"hello from x86jit\n");

    let interp = run(&cfg, &rootfs, EngineKind::Interpreter, &a);
    let jit = run(&cfg, &rootfs, EngineKind::Jit, &a);
    assert_eq!(interp.stdout, native.stdout, "interp == native");
    assert_eq!(jit.stdout, native.stdout, "jit == native");
    assert_eq!(interp.exit_code, Some(0));
    assert_eq!(jit.exit_code, Some(0));
}

/// `busybox sha256sum <file>` — reads a file through the rootfs guest filesystem;
/// the digest must be correct and identical on interpreter and JIT.
#[test]
fn busybox_sha256sum_reads_rootfs_three_ways() {
    let (cfg, rootfs) = setup("sha");
    std::fs::write(rootfs.join("data.txt"), b"hello world\n").unwrap();
    let a = ["/bin/busybox", "sha256sum", "/data.txt"];
    // sha256 of "hello world\n"
    const DIGEST: &str = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";

    let interp = run(&cfg, &rootfs, EngineKind::Interpreter, &a);
    let jit = run(&cfg, &rootfs, EngineKind::Jit, &a);
    assert_eq!(interp.stdout, jit.stdout, "interp == jit");
    assert_eq!(interp.exit_code, Some(0));

    let out = String::from_utf8_lossy(&interp.stdout);
    let digest = out.split_whitespace().next().unwrap_or("");
    assert_eq!(digest, DIGEST, "sha256sum digest via the rootfs");
}
