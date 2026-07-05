//! ISA compatibility map CLI (OCI-0.T1).
//!
//! ```text
//! cargo run -p x86jit-tests --bin compat -- --write   # regenerate wiki/compat/*
//! cargo run -p x86jit-tests --bin compat              # print the dashboard
//! ```
//!
//! `--write` refreshes the checked-in `wiki/compat/coverage.json` +
//! `isa-coverage.md` after a lift arm is added. The `compat_map_is_current` test
//! fails until they're refreshed, so the map cannot rot.

use x86jit_tests::compat::compute_coverage;

fn main() {
    let write = std::env::args().any(|a| a == "--write");
    let cov = compute_coverage();
    if write {
        cov.write_artifacts().expect("write artifacts");
        println!(
            "wrote {}/{{coverage.json,isa-coverage.md}}",
            x86jit_tests::compat::artifact_dir().display()
        );
    } else {
        print!("{}", cov.to_markdown());
    }
}
