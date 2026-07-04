//! Reference output for whole-program differential tests.
//!
//! The whole-program tests assert `native == interpreter == JIT`. The "native"
//! leg executes the guest **x86-64** binary directly on the host — which only
//! works when the host *is* x86-64. On an ARM CI runner we can't exec an x86-64
//! ELF, but interp and JIT still emulate it, and comparing them against a known
//! expected output is exactly what validates the AArch64 JIT backend.
//!
//! So on x86-64 we run the binary natively and assert it matches the baked
//! expectation (catching a stale fixture); on any other host we skip the native
//! leg and use the baked expectation as the reference. Either way interp and JIT
//! are checked against the same bytes.

/// Resolve the reference output for a whole-program test.
///
/// On x86-64, `run_native` is invoked and its output asserted equal to
/// `expected` before being returned. On other hosts, `run_native` is never
/// called and `expected` is returned verbatim.
pub fn reference(expected: &[u8], run_native: impl FnOnce() -> Vec<u8>) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        let native = run_native();
        assert_eq!(
            native, expected,
            "native output != baked expectation (stale fixture?)"
        );
        native
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = run_native;
        expected.to_vec()
    }
}

/// Like [`reference`], but for a *dynamically-linked* guest binary whose native
/// execution needs the host to provide the guest's ELF interpreter (e.g.
/// `/lib/ld-musl-x86_64.so.1`). A CI runner often lacks that loader, so a spawn
/// failure (`ENOENT` from the missing interpreter) is tolerated: we fall back to
/// the baked expectation with a note. A binary that *does* run but produces the
/// wrong bytes still fails — only an unavailable loader is excused.
pub fn reference_dyn(
    expected: &[u8],
    run_native: impl FnOnce() -> std::io::Result<Vec<u8>>,
) -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        match run_native() {
            Ok(native) => {
                assert_eq!(
                    native, expected,
                    "native output != baked expectation (stale fixture?)"
                );
                native
            }
            Err(e) => {
                eprintln!("skipping native leg (guest dynamic loader unavailable): {e}");
                expected.to_vec()
            }
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = run_native;
        expected.to_vec()
    }
}
