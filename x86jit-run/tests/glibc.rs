//! OCI-3: the real busybox:glibc image (a dynamic glibc PIE) runs applets three
//! ways (native == interpreter == JIT). Exercises the dynamic loader path
//! (ld-linux + libc.so loaded from the rootfs via GuestFs) and fxsave/fxrstor,
//! which glibc's PLT resolver uses to preserve XMM across symbol resolution.

mod common;
use common::{oci, Native};

#[test]
fn busybox_glibc_echo_runs_three_ways() {
    oci("busybox-glibc.tar", "gl-echo")
        .argv(&["/bin/busybox", "echo", "dynamic glibc on x86jit"])
        .native(Native::Host)
        .expect_stdout(b"dynamic glibc on x86jit\n")
        .expect_exit(0)
        .run();
}

#[test]
fn busybox_glibc_sha256sum_reads_rootfs() {
    const DIGEST: &str = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";
    let ran = oci("busybox-glibc.tar", "gl-sha")
        .file("data.txt", b"hello world\n")
        .argv(&["/bin/busybox", "sha256sum", "/data.txt"])
        .run();
    assert_eq!(ran.first_token(), DIGEST);
}
