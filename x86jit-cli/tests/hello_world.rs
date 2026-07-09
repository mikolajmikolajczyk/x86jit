//! OCI-1 acceptance: the real `hello-world` Docker image runs three ways
//! (native == interpreter == JIT). Proves the whole pipeline end to end — registry
//! pull → rootfs + config → ELF load → engine → syscall shim — on an unmodified
//! upstream container image.

mod common;
use common::{oci, Native, HELLO_WORLD};

#[test]
fn hello_world_image_runs_three_ways() {
    let Some(ran) = oci(HELLO_WORLD, "hello")
        .native(Native::Host) // static entrypoint, runs on the host directly
        .expect_exit(0)
        .run()
    else {
        return;
    };
    assert!(
        ran.stdout().starts_with(b"\nHello from Docker!"),
        "prints the greeting"
    );
}
