//! `x86jit-run` — run an OCI/Docker image on the x86jit recompiler.
//!
//! ```text
//! x86jit-run <image.tar> [--backend interp|jit|both]
//! ```
//!
//! Offline: pass a `docker save`d tarball. The rootfs is extracted to a temp dir.

use std::path::{Path, PathBuf};

use x86jit_run::{run_image, EngineKind, RunResult};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(image) = args.first() else {
        eprintln!("usage: x86jit-run <image.tar> [--backend interp|jit|both]");
        std::process::exit(2);
    };
    let backend = args
        .iter()
        .position(|a| a == "--backend")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
        .unwrap_or("jit");

    let engines: &[EngineKind] = match backend {
        "interp" => &[EngineKind::Interpreter],
        "jit" => &[EngineKind::Jit],
        "both" => &[EngineKind::Interpreter, EngineKind::Jit],
        other => {
            eprintln!("unknown backend {other:?} (interp|jit|both)");
            std::process::exit(2);
        }
    };

    let mut last: Option<RunResult> = None;
    for &engine in engines {
        let rootfs = scratch_dir(engine);
        match run_image(Path::new(image), &rootfs, engine) {
            Ok(res) => {
                if engines.len() > 1 {
                    eprintln!("--- {engine:?} ---");
                }
                use std::io::Write;
                std::io::stdout().write_all(&res.stdout).unwrap();
                if let Some(prev) = &last {
                    if prev != &res {
                        eprintln!("MISMATCH between backends");
                        std::process::exit(1);
                    }
                }
                last = Some(res);
            }
            Err(e) => {
                eprintln!("{engine:?}: {e}");
                std::process::exit(1);
            }
        }
    }
    if let Some(code) = last.and_then(|r| r.exit_code) {
        std::process::exit(code);
    }
}

fn scratch_dir(engine: EngineKind) -> PathBuf {
    let d = std::env::temp_dir().join(format!("x86jit-run-{engine:?}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
