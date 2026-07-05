//! OCI-3: the real alpine image (a musl-dynamic busybox) runs `cat /etc/os-release`
//! three ways. Exercises the ld-musl dynamic loader path and graceful -ENOSYS for
//! syscalls busybox probes then falls back from (sendfile -> read/write).

mod common;
use common::{oci, Native};

#[test]
fn alpine_cat_os_release_interp_eq_jit() {
    // Native skipped: a musl-dynamic busybox may not run on a glibc host, so the
    // rootfs file's own content is the oracle.
    let ran = oci("alpine.tar", "alpine-cat")
        .argv(&["/bin/busybox", "cat", "/etc/os-release"])
        .native(Native::Skip)
        .expect_exit(0)
        .run();
    let expected = std::fs::read(ran.rootfs.join("etc/os-release")).expect("os-release");
    assert_eq!(ran.stdout(), expected, "cat output == the file");
    assert!(ran.stdout().starts_with(b"NAME=\"Alpine Linux\""));
}
