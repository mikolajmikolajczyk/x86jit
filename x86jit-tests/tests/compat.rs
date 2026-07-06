//! ISA compatibility map enforcement (OCI-0.T1): the checked-in map cannot rot.
//! Adding a lift arm without refreshing `backlog/docs/compat/coverage.json` fails this
//! test — the map is always the truth about what the lifter handles.

use x86jit_tests::compat::{
    advertised_simd_features, compute_coverage, cpuid_waivers, feature_coverage, Coverage,
};

#[test]
fn compat_map_is_current() {
    let fresh = compute_coverage().to_json();
    let checked_in = std::fs::read_to_string(
        x86jit_tests::compat::artifact_dir().join("coverage.json"),
    )
    .expect("backlog/docs/compat/coverage.json missing — run: cargo run -p x86jit-tests --bin compat -- --write");

    assert_eq!(
        fresh, checked_in,
        "ISA compat map is stale. The lifter's coverage changed but \
         backlog/docs/compat/coverage.json wasn't regenerated. Run:\n  \
         cargo run -p x86jit-tests --bin compat -- --write\nand commit the result."
    );
}

/// CPUID must not advertise a feature the lifter can't fully execute (OCI-0.T2). A
/// guest's CPUID-dispatched path (esp. glibc IFUNC resolvers) jumps straight into
/// the instruction after seeing its bit, so an advertised-but-unimplemented feature
/// is a live trap. Every advertised feature must be either 100% lifted or listed in
/// `compat/cpuid-waivers.ron` with a reason. Advertising a new feature without
/// implementing or waiving it fails here.
#[test]
fn cpuid_advertises_only_what_lifts() {
    let waived: std::collections::HashSet<String> =
        cpuid_waivers().into_iter().map(|(f, _)| f).collect();

    let mut failures = Vec::new();
    for f in advertised_simd_features() {
        let name = format!("{f:?}");
        let (_lifted, missing) = feature_coverage(f);
        if !missing.is_empty() && !waived.contains(&name) {
            let sample: Vec<_> = missing.iter().take(5).collect();
            failures.push(format!(
                "{name}: {} advertised codes not lifted, e.g. {sample:?}",
                missing.len()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "CPUID advertises features the lifter doesn't fully implement. Either implement \
         them (see backlog/docs/compat/isa-coverage.md) or add a reasoned waiver to \
         x86jit-tests/compat/cpuid-waivers.ron:\n{}",
        failures.join("\n")
    );
}

/// A waiver must name a real advertised feature — a stale waiver (feature no longer
/// advertised, or now fully lifted) should be removed, not linger.
#[test]
fn cpuid_waivers_are_not_stale() {
    let advertised: std::collections::HashSet<String> = advertised_simd_features()
        .into_iter()
        .map(|f| format!("{f:?}"))
        .collect();

    for (feat, _reason) in cpuid_waivers() {
        assert!(
            advertised.contains(&feat),
            "waiver for `{feat}` but that feature isn't advertised by cpuid_run — remove it"
        );
        let iced_feat = advertised_simd_features()
            .into_iter()
            .find(|f| format!("{f:?}") == feat)
            .expect("advertised");
        let (_lifted, missing) = feature_coverage(iced_feat);
        assert!(
            !missing.is_empty(),
            "waiver for `{feat}` but it's now fully lifted — remove the waiver"
        );
    }
}

/// Sanity: the probe actually measures something (guards against a silently-broken
/// probe reporting everything as unencodable, which would make the map meaningless).
#[test]
fn probe_measures_real_coverage() {
    let cov: Coverage = Coverage::load_checked_in()
        .expect("coverage.json missing — run the compat bin with --write");
    let v1 = cov.generations.get("x86-64-v1").expect("v1 row present");
    assert!(
        v1.lifted > 100,
        "v1 baseline should have many lifted instructions, got {}",
        v1.lifted
    );
}
