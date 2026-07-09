//! Architectural boundary tripwire (OCI-0.T3, spec §1/§4.1): `x86jit-core` is a
//! guest-agnostic recompiler. File-format parsing, OS syscall emulation, the
//! process model, and devices live in embedder crates, never in core. Embedder
//! crates (`x86jit-linux`, `x86jit-cli`'s `oci` module, …) exist precisely to keep
//! that line — this test turns the sacred rule into a red build instead of a review
//! hope: core's dependency set must stay exactly `{iced-x86}` (the x86 decoder,
//! the one thing a recompiler legitimately needs). Adding tar/JSON/serde/nix/etc.
//! to core is what this catches. A second test pins the `oci` image reader (a
//! `x86jit-cli` module since the run/oci/cli merge) core-free, so it can never leak
//! into the recompiler even though it no longer lives in its own crate.

use std::path::Path;

/// Parse the `[dependencies]` table of a Cargo.toml into the set of crate names,
/// ignoring `[dev-dependencies]`, `[build-dependencies]`, and `[target.*]` tables.
fn dependency_names(cargo_toml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_deps = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            // A new table starts; we only care about the exact `[dependencies]`.
            in_deps = trimmed == "[dependencies]";
            continue;
        }
        if !in_deps || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // `name = ...` or `name.workspace = true` — the crate name is the token
        // before the first `.` or `=`.
        let key = trimmed
            .split_once('=')
            .map(|(k, _)| k.trim())
            .unwrap_or(trimmed);
        let name = key.split('.').next().unwrap_or(key).trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
}

#[test]
fn core_stays_guest_agnostic() {
    // This test lives in x86jit-tests; the core manifest is two dirs up.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("x86jit-core")
        .join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));

    let deps = dependency_names(&text);
    assert_eq!(
        deps,
        vec!["iced-x86".to_string()],
        "x86jit-core must depend ONLY on the x86 decoder (iced-x86). Anything else \
         (tar/JSON/serde/OS crates) belongs in an embedder crate (x86jit-linux / \
         x86jit-cli), not the guest-agnostic core (spec §1/§4.1). Found: {deps:?}"
    );
}

/// The `oci` image reader (folded into `x86jit-cli` from the former `x86jit-oci`
/// crate) must stay free of any `x86jit_core` reference — reading a `docker save`
/// image has nothing to do with the recompiler (spec §1/§4.1). It's no longer a
/// separate crate, so the compiler can't enforce it; this source tripwire does.
#[test]
fn oci_module_stays_core_free() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("x86jit-cli")
        .join("src")
        .join("oci.rs");
    let text =
        std::fs::read_to_string(&src).unwrap_or_else(|e| panic!("read {}: {e}", src.display()));
    assert!(
        !text.contains("x86jit_core"),
        "x86jit-cli/src/oci.rs must not reference x86jit_core — the image reader is a \
         guest-agnostic embedder piece (spec §1/§4.1). Keep it a pure `docker save` parser."
    );
}
