//! OCI-4 (start): a shell entrypoint that `execve`s its command. `busybox sh -c
//! <cmd>` replaces the process image with the command directly (no fork for a
//! single command), which the runner fulfills by reloading a fresh process image.
//! Three ways: native == interpreter == JIT.

use std::path::{Path, PathBuf};

use x86jit_oci::{load_image, ImageConfig};
use x86jit_run::{run_config_argv, EngineKind, RunResult};

const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-oci/fixtures/busybox-musl.tar"
);

fn setup(name: &str) -> (ImageConfig, PathBuf) {
    let rootfs = std::env::temp_dir().join(format!("x86jit-sh-{name}"));
    let _ = std::fs::remove_dir_all(&rootfs);
    std::fs::create_dir_all(&rootfs).unwrap();
    let cfg = load_image(Path::new(IMAGE), &rootfs).expect("load image");
    (cfg, rootfs)
}

fn run(cfg: &ImageConfig, rootfs: &Path, e: EngineKind, script: &str) -> RunResult {
    let argv: Vec<String> = ["/bin/busybox", "sh", "-c", script]
        .iter()
        .map(|s| s.to_string())
        .collect();
    run_config_argv(cfg, rootfs, e, &argv).expect("run sh")
}

fn native(rootfs: &Path, script: &str) -> Vec<u8> {
    std::process::Command::new(rootfs.join("bin/busybox"))
        .args(["sh", "-c", script])
        .output()
        .expect("native busybox sh")
        .stdout
}

/// A shell builtin (`echo`) — single process, no exec.
#[test]
fn sh_builtin_three_ways() {
    let (cfg, rootfs) = setup("builtin");
    let script = "echo hello from the shell";
    let want = native(&rootfs, script);
    assert_eq!(want, b"hello from the shell\n");
    let i = run(&cfg, &rootfs, EngineKind::Interpreter, script);
    let j = run(&cfg, &rootfs, EngineKind::Jit, script);
    assert_eq!(i.stdout, want, "interp == native");
    assert_eq!(j.stdout, want, "jit == native");
}

/// An external command — the shell `execve`s it (process replacement, no fork).
/// The native leg is skipped: the guest resolves `/bin/busybox` inside the image
/// rootfs, but a host subprocess resolves it on the host filesystem (no chroot), so
/// interp == JIT against the known output is the oracle — the execve *mechanism* is
/// what's under test.
#[test]
fn sh_execve_command_interp_eq_jit() {
    let (cfg, rootfs) = setup("exec");
    let script = "/bin/busybox echo executed via execve";
    let i = run(&cfg, &rootfs, EngineKind::Interpreter, script);
    let j = run(&cfg, &rootfs, EngineKind::Jit, script);
    assert_eq!(i.stdout, j.stdout, "interp == jit");
    assert_eq!(i.stdout, b"executed via execve\n", "the exec'd command ran");
    assert_eq!((i.exit_code, j.exit_code), (Some(0), Some(0)));
}

/// A pipeline: `echo hello | cat` forks two children joined by a pipe (OCI-4).
/// Exercises pipe + fork + wait4 + fd inheritance across the whole runner. Native
/// leg skipped (same rootfs-path reason as the execve test); interp == jit against
/// the known output is the oracle.
#[test]
fn sh_pipeline_echo_cat_interp_eq_jit() {
    let (cfg, rootfs) = setup("pipeline");
    let script = "echo hello | cat";
    let i = run(&cfg, &rootfs, EngineKind::Interpreter, script);
    let j = run(&cfg, &rootfs, EngineKind::Jit, script);
    assert_eq!(i.stdout, j.stdout, "interp == jit");
    assert_eq!(i.stdout, b"hello\n", "the pipeline delivered echo's output through cat");
    assert_eq!((i.exit_code, j.exit_code), (Some(0), Some(0)));
}

/// Command substitution `$(...)`: the shell forks a child, captures its stdout via a
/// pipe, and splices it into the command line — fork + pipe + wait with the parent
/// as the reader (spec §6).
#[test]
fn sh_command_substitution_interp_eq_jit() {
    let (cfg, rootfs) = setup("cmdsub");
    let script = "echo out-$(echo inner)";
    let i = run(&cfg, &rootfs, EngineKind::Interpreter, script);
    let j = run(&cfg, &rootfs, EngineKind::Jit, script);
    assert_eq!(i.stdout, j.stdout, "interp == jit");
    assert_eq!(i.stdout, b"out-inner\n", "substitution spliced the child's output");
}

/// A two-stage pipeline through a real applet: `printf` (builtin) into `grep` (an
/// external applet the child execve's).
#[test]
fn sh_pipeline_grep_interp_eq_jit() {
    let (cfg, rootfs) = setup("grep");
    let script = "printf 'a\\nb\\nc\\n' | grep b";
    let i = run(&cfg, &rootfs, EngineKind::Interpreter, script);
    let j = run(&cfg, &rootfs, EngineKind::Jit, script);
    assert_eq!(i.stdout, j.stdout, "interp == jit");
    assert_eq!(i.stdout, b"b\n", "grep selected the matching line from the pipe");
}
