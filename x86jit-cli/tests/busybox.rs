//! OCI-2 acceptance: the real `busybox:musl` image (a static-PIE single binary)
//! runs applets directly — no `sh -c`, no fork — three ways (native == interpreter
//! == JIT). Exercises static-PIE loading, the rootfs guest filesystem, and the
//! static-musl syscall surface.

mod common;
use common::{oci, Native};

/// `busybox echo` — plain applet, three ways, full stdout must match.
#[test]
fn busybox_echo_runs_three_ways() {
    oci("busybox-musl.tar", "bb-echo")
        .argv(&["/bin/busybox", "echo", "hello from x86jit"])
        .native(Native::Host)
        .expect_stdout(b"hello from x86jit\n")
        .expect_exit(0)
        .run();
}

/// `busybox sha256sum <file>` — reads a file through the rootfs guest filesystem;
/// the digest must be correct and identical on interpreter and JIT.
#[test]
fn busybox_sha256sum_reads_rootfs_three_ways() {
    // sha256 of "hello world\n"
    const DIGEST: &str = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";
    let ran = oci("busybox-musl.tar", "bb-sha")
        .file("data.txt", b"hello world\n")
        .argv(&["/bin/busybox", "sha256sum", "/data.txt"])
        .expect_exit(0)
        .run();
    assert_eq!(ran.first_token(), DIGEST, "sha256sum digest via the rootfs");
}
