---
id: TASK-265
title: >-
  BUG: f32_to_f16 directed-rounding wrong at underflow (vcvtps2ph) —
  native-vs-interp divergence, fuzz seed 1964
status: In Progress
assignee: []
created_date: '2026-07-17 14:16'
updated_date: '2026-07-17 20:01'
labels:
  - bug
  - simd
  - fuzz
dependencies: []
ordinal: 295000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found by the AVX fuzz driver (TASK-264, `fuzz_avx`). The interpreter diverges from the real host CPU (NativeOracle — ground truth for VEX) on `vcvtps2ph` when the imm8 selects a *directed* rounding mode (toward +inf / -inf) rather than round-to-nearest. The bug is in `f32_to_f16` (`x86jit-core/src/interp/mod.rs:2626`).

Witness (137k-program run, `fuzz-avx-findings.log`):

    === native-vs-interp divergence (seed 1964) ===
    ops: [ VVex { op: 62, dst: 4, a: 2, b: 4, imm: 226 } ]
      xmm4: expected 0x00010001fbff8000764944d680000001
            got      0x00000000fbff8000764944d680000000

imm8 = 226 = 0b1110_0010 -> imm[2]=0 (use imm[1:0] as RC), imm[1:0]=0b10 = round toward +inf.
Two lanes: hardware yields 0x0001 (smallest f16 subnormal), interp yields 0x0000. Under round-toward-+inf every tiny *positive* f32 must round UP to the smallest representable magnitude, never flush to zero.

Root cause, plus two more defects found by inspecting the same function:

1. **Underflow ignores RC (the proven bug).** Line ~2673:

       if e < -10 { return sign; } // too small -> +/-0

   Unconditional flush-to-zero. Under RC=toward-+inf a positive input must give 0x0001; under RC=toward--inf a negative input must give 0x8001. Only nearest-even / toward-zero may return a true zero here.

2. **Subnormal round-up carry is masked away.** Line ~2679:

       return sign | (m as u16 & 0x3ff);

   `round()` can carry `m` to 0x400. In the IEEE binary16 encoding 0x400 is exactly the smallest *normal* (exp field 1, mantissa 0), so `sign | m` is already correct by construction — the `& 0x3ff` mask drops the carry and wraps the largest subnormal back to zero. Suspected, not yet witnessed by the fuzzer.

3. **Overflow path is self-admittedly hand-waved.** Lines ~2658-2668 carry the comment "Keep it simple and correct" and a `// hardware -> inf` question mark on the RC=toward--inf case. Re-derive it from the SDM rather than reasoning inline. An earlier smoke finding showed interp `fbff` (max finite, -65504) where hardware gave `fc00` (-inf); the seed-1964 witness above happens to agree on its `fbff` lane, so the overflow rule is unproven either way and must be settled against the native oracle, not by reading the code.

Scope: interpreter only (`f32_to_f16`). The JIT reaches the same function through the `cvtps2ph` helper, so jit == interp holds and the JIT needs no separate fix — this is a semantics bug against hardware, which is why only the native leg caught it.

Reproduce: `FUZZ_SECONDS=60 FUZZ_START=1964 cargo test --release -p x86jit-tests --test fuzz_avx -- --ignored --nocapture`
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 f32_to_f16 honours RC in the underflow path: a positive f32 too small for a subnormal returns 0x0001 under round-toward-+inf, and a negative one returns 0x8001 under round-toward--inf; both still return signed zero under nearest-even and toward-zero
- [ ] #2 The subnormal round-up carry into 0x400 produces the smallest normal (0x0400 / 0x8400), not zero
- [ ] #3 The overflow-to-inf rule for all four RC values is re-derived from the Intel SDM VCVTPS2PH pseudocode and matches the NativeOracle on both signs
- [ ] #4 A native-vs-interp test in x86jit-tests/src/native.rs drives vcvtps2ph across all four imm8 RC modes with overflow, underflow, subnormal-boundary and zero inputs, and is proven to FAIL without the fix
- [ ] #5 fuzz_avx run of >=137k programs from seed 1 reports zero op-62 native-vs-interp divergences
- [ ] #6 cargo nextest run -E 'not binary(fuzz_robustness)' and cargo clippy --all-targets --all-features -- -D warnings both pass
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed by agent + a 4th defect caught during the combined gate. Agent fixed 3 defects in f32_to_f16 (underflow RC, subnormal 0x400 carry, overflow). During validation the new cargo xfuzz replay surfaced a 4th: lift_vcvtps2ph (lift/vector.rs:4643) passed rc = imm8 & 0x7, but imm8[2]=1 selects MXCSR rounding (round-nearest) and imm8[1:0] must be ignored. Fix: rc = if imm & 0x4 != 0 { 0 } else { imm & 0x3 }. Witness fuzz seed 88 (imm=95, imm[2]=1): interp used toward-zero -> fbff (max finite) where hardware uses MXCSR-nearest -> fc00 (-inf). Fix at the lift covers both tiers (shared IR). Native test native_vcvtps2ph_directed_rounding_boundaries_match_interp extended with imm=4 and imm=7 (imm[2]=1 cases), proven RED without the lift fix. Targeted sweep: cargo xfuzz --ops vcvtps2ph 30s = 10615 progs, 1188 native runs, 0 divergences. Combined gate: 659 nextest passed, clippy clean. Ready for review; not committed.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
