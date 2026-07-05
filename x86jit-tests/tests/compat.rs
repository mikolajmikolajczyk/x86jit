//! ISA compatibility map enforcement (OCI-0.T1): the checked-in map cannot rot.
//! Adding a lift arm without refreshing `wiki/compat/coverage.json` fails this
//! test — the map is always the truth about what the lifter handles.

use x86jit_tests::compat::{compute_coverage, Coverage};

#[test]
fn compat_map_is_current() {
    let fresh = compute_coverage().to_json();
    let checked_in = std::fs::read_to_string(
        x86jit_tests::compat::artifact_dir().join("coverage.json"),
    )
    .expect("wiki/compat/coverage.json missing — run: cargo run -p x86jit-tests --bin compat -- --write");

    assert_eq!(
        fresh, checked_in,
        "ISA compat map is stale. The lifter's coverage changed but \
         wiki/compat/coverage.json wasn't regenerated. Run:\n  \
         cargo run -p x86jit-tests --bin compat -- --write\nand commit the result."
    );
}

/// Sanity: the probe actually measures something (guards against a silently-broken
/// probe reporting everything as unencodable, which would make the map meaningless).
#[test]
fn probe_measures_real_coverage() {
    let cov: Coverage = Coverage::load_checked_in()
        .expect("coverage.json missing — run the compat bin with --write");
    let v1 = cov
        .generations
        .get("x86-64-v1")
        .expect("v1 row present");
    assert!(
        v1.lifted > 100,
        "v1 baseline should have many lifted instructions, got {}",
        v1.lifted
    );
}
