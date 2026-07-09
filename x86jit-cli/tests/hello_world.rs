//! OCI-1 acceptance: the real `hello-world` Docker image runs three ways
//! (native == interpreter == JIT). Proves the whole pipeline end to end — image
//! tar → rootfs + config → ELF load → engine → syscall shim — on an unmodified
//! upstream container image.

mod common;
use common::{oci, Native};

#[test]
fn hello_world_image_runs_three_ways() {
    let ran = oci("hello-world.tar", "hello")
        .native(Native::Host) // static entrypoint, runs on the host directly
        .expect_exit(0)
        .run();
    assert!(
        ran.stdout().starts_with(b"\nHello from Docker!"),
        "prints the greeting"
    );
}
