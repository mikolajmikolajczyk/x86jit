//! Parse the vendored `hello-world` image tar: config + extracted rootfs.

use std::path::Path;
use x86jit_oci::load_image;

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("x86jit-oci-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn parses_config_and_extracts_rootfs() {
    let tar = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/hello-world.tar");
    // Distinct scratch name from the x86jit-run three-ways test (`oci(.., "hello")`):
    // both binaries run in parallel and would otherwise race on the same temp rootfs
    // (`remove_dir_all` + extract), intermittently losing `/hello`.
    let rootfs = scratch("hello-parse");
    let cfg = load_image(&tar, &rootfs).expect("load hello-world");

    assert_eq!(cfg.architecture, "amd64");
    assert_eq!(cfg.os, "linux");
    assert_eq!(cfg.argv(), vec!["/hello".to_string()], "Cmd is /hello");
    assert_eq!(cfg.working_dir, "/");
    assert!(
        cfg.env.iter().any(|e| e.starts_with("PATH=")),
        "has PATH env"
    );

    // The single layer drops a static ELF at /hello.
    let hello = rootfs.join("hello");
    assert!(hello.exists(), "rootfs/hello extracted");
    let bytes = std::fs::read(&hello).unwrap();
    assert_eq!(&bytes[..4], b"\x7fELF", "the entrypoint is an ELF");
}
