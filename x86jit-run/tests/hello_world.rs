//! OCI-1 acceptance: the real `hello-world` Docker image runs three ways
//! (native == interpreter == JIT). Proves the whole pipeline end to end — image
//! tar → rootfs + config → ELF load → engine → syscall shim — on an unmodified
//! upstream container image.

use std::path::{Path, PathBuf};

use x86jit_oci::load_image;
use x86jit_run::{run_config, EngineKind};

const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-oci/fixtures/hello-world.tar"
);

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("x86jit-run-test-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn hello_world_image_runs_three_ways() {
    let rootfs = scratch("hello");
    let cfg = load_image(Path::new(IMAGE), &rootfs).expect("load image");

    // Native: run the extracted static entrypoint directly on the host.
    let entry = rootfs.join(cfg.argv()[0].trim_start_matches('/'));
    let native = std::process::Command::new(&entry)
        .output()
        .expect("run native /hello");
    assert!(native.status.success(), "native exit");
    assert!(
        native.stdout.starts_with(b"\nHello from Docker!"),
        "sanity: native prints the greeting"
    );

    // Interpreter and JIT via the recompiler.
    let interp = run_config(&cfg, &rootfs, EngineKind::Interpreter).expect("interp run");
    let jit = run_config(&cfg, &rootfs, EngineKind::Jit).expect("jit run");

    assert_eq!(interp.stdout, native.stdout, "interpreter == native");
    assert_eq!(jit.stdout, native.stdout, "JIT == native");
    assert_eq!(interp.exit_code, Some(0));
    assert_eq!(jit.exit_code, Some(0));
}
