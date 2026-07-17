---
id: TASK-271
title: >-
  Fuzz campaign: tolerate unspecified NaN sign/payload in the native-vs-interp
  compare (FMA, cvtps2ph)
status: In Progress
assignee: []
created_date: '2026-07-17 20:33'
updated_date: '2026-07-17 21:14'
labels:
  - fuzz
  - tooling
dependencies:
  - TASK-268
ordinal: 301000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
After TASK-266 removed the has_legacy_vec filter (native leg now ~100% coverage) and TASK-268 added FP corner operands, the AVX fuzz campaign frequently surfaces native-vs-interp divergences that are NOT correctness bugs: the x86 SDM leaves the SIGN and PAYLOAD of a computed QNaN architecturally UNSPECIFIED, so the real CPU and the interpreter softfloat legitimately differ on those bits.

Two observed classes (both benign, both re-surface every run):
- FMA: vfmadd/vfmaddsub/vfmsubadd 213 ps/pd — the NaN sign bit of a fused-vs-unfused result.
- vcvtps2ph: e.g. seed 51 → native 0x7e00 vs interp 0x7e01, both QNaNs (the low payload bit). The TASK-265 rounding fix is unaffected — the diverging lane is a NaN, not a rounded finite value.

These drown out real findings in the log. The campaign native compare should treat two values as equal when both are NaN of the same class (both QNaN, or both SNaN) at the same lane, ignoring sign and payload — WITHOUT masking a real bug (a finite-vs-NaN mismatch, or NaN-vs-different-class, must still fail).

Scope: the fuzz harness compare only — x86jit-tests (compare.rs / dontcare_flags / the campaign leg in fuzz.rs / possibly a NaN-aware vector compare in native.rs). Do NOT change interpreter semantics (the interp NaN output is a legitimate choice; we are not trying to bit-match hardware NaN payloads, which are unspecified). Per-lane, per-element-width (f16/f32/f64) NaN-class equivalence.

Rationale: with the filter gone and FP corners dense, an un-tolerant compare makes the native leg noisy enough to hide the next real float bug. This is the noise-reduction that makes long float sweeps actionable.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The campaign native-vs-interp compare treats same-lane same-class NaNs (both QNaN or both SNaN) as equal regardless of sign/payload, per element width (f16/f32/f64)
- [ ] #2 A finite-vs-NaN, NaN-vs-finite, or QNaN-vs-SNaN mismatch at a lane STILL fails (no real bug is masked)
- [ ] #3 Interpreter/JIT semantics are untouched — change is confined to x86jit-tests compare/harness
- [ ] #4 A targeted sweep (cargo xfuzz --family convert,fma --secs 60) reports zero NaN-sign/payload-only divergences while still catching an injected finite-value divergence in a smoke check
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: compare_nan_tolerant + nan_payload_equiv(a,b,widths) in compare.rs, wired into both campaign legs (fuzz.rs) with per-program float widths from VexOp::fp_widths()/prog_fp_widths. SAFE design: tries only the float widths the program uses (via fp_widths) so an f32 ±inf sign-flip cannot alias an f16 NaN; a quiet-vs-signaling class mismatch at any tried width VETOES tolerance (hardware only emits qNaN). Unit tests (nan_tests) cover same-class-tolerated + finite/class/inf-mismatch-rejected. Pure same-class-NaN-payload divergences now tolerated; the residual convert,fma divergences turned out to be REAL FMA bugs (subnormal/inf-sign/NaN-quieting) → filed TASK-272, not masked. Ready for review; not committed.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
