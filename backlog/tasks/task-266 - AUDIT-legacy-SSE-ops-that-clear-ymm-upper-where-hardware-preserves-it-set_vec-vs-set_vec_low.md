---
id: TASK-266
title: >-
  AUDIT: legacy-SSE ops that clear ymm upper where hardware preserves it
  (set_vec vs set_vec_low)
status: In Progress
assignee: []
created_date: '2026-07-17 19:12'
updated_date: '2026-07-17 19:20'
labels:
  - audit
  - simd
  - fuzz
dependencies: []
ordinal: 296000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The AVX fuzz driver (TASK-264) skips any program containing a legacy-SSE vector op on its native-oracle leg, via has_legacy_vec() in x86jit-tests/tests/fuzz_avx.rs. The justifying comment there claims x86jit models legacy SSE as clearing bits 255:128 — 'a documented model choice'. **That claim is unverified and probably wrong.** It appears nowhere in spec.md, conventions.md, or deferred.md; it is only asserted in that fuzz comment (written by me while silencing a noisy log).

Inspection contradicts it: x86jit-core/src/interp/vector.rs uses set_vec_low (preserves upper — CORRECT for legacy SSE) in 6 places, each with the comment 'SSE preserves upper; VEX.128 zeroes via VZeroUpper', and set_vec (zero-extends, CLEARS upper) in 19 places. So there is no blanket model — the rule is applied correctly in some lifts and (suspected) wrong in others. Every legacy-SSE op that reaches set_vec and writes < 16 bytes, or that should preserve the upper 128, is a candidate real bug that the fuzz filter is currently hiding.

Real hardware: a legacy (non-VEX) SSE instruction writes only bits 127:0 of the destination XMM and PRESERVES bits 255:128 (and higher). Only the VEX/EVEX encodings zero the upper. x86jit must match this on the interpreter (jit==interp means the JIT inherits whatever interp does).

Goal of THIS task: enumerate, don't fix. Produce a verdict per legacy-SSE op in the fuzz pools (the vbin/vnew/vshuf/vshift_imm tables in x86jit-tests/src/fuzz.rs) of whether interp clears the upper where hardware preserves it, using the NativeOracle (x86jit-tests/src/native.rs) with a pre-dirtied ymm upper. File a follow-up bug task per confirmed divergence. Also settle whether has_legacy_vec is a legitimate noise filter or a bug-silencer, and record the answer in spec.md/conventions.md so the model choice is actually documented (or corrected).

Method: for each op, seed ymm_hi[dst] = nonzero, run the single op, compare interp vs NativeOracle upper 128 bits. Divergence where hardware upper is preserved and interp upper is zeroed = confirmed bug.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every legacy-SSE op in the fuzz vbin/vnew/vshuf/vshift_imm pools has a recorded verdict: preserves-upper (matches hw) or clears-upper (diverges), determined against the NativeOracle with a pre-dirtied ymm upper
- [ ] #2 A follow-up bug task is filed for each confirmed clears-where-hw-preserves divergence, citing the specific interp function and the set_vec call site
- [ ] #3 The legacy-SSE upper-half semantics are documented in spec.md and/or conventions.md (the §16 semantics-traps home), replacing the unverified claim currently living only in the fuzz_avx.rs comment
- [ ] #4 has_legacy_vec in fuzz_avx.rs is either justified in-comment by the documented rule, or removed/narrowed once the underlying bugs are tracked, so the native leg stops silently dropping ~83% of programs for a non-reason
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Audit complete (empirical native-oracle probe, x86jit-tests/tests/legacy_upper_audit.rs, #[ignore]d). 62 legacy-SSE ops probed with a pre-dirtied ymm upper: native CPU preserved 255:128 on ALL 62; interp wrongly ZEROED on exactly 2 instructions — packsswb and packssdw (signed packs). All 60 others (packed-int add/sub/cmp/unpack/pack-unsigned/minmax/sat/avg/pmaddwd, logic, round*, hadd/hsub/addsub, phadd*/phsub*, psadbw) correctly preserve. Root cause: exec_vpack (interp/mod.rs:4744) uses set_vec (zero-extends) on the shared VPackWide IR op; VPackWide is shared by legacy(16,preserve) / VEX.128(16,clear) / VEX.256(32,clear), so the decision must move to the lift. Fix filed as TASK-269.

Verdict on has_legacy_vec (fuzz_avx.rs): the "documented model choice" comment was FALSE — interp does NOT blanket-clear legacy upper; it preserves correctly except for the 2 pack bugs. So the filter was dropping ~83% of native-leg programs for a non-reason (60 ops it skips are already hardware-correct). Plan once TASK-269 lands: DELETE has_legacy_vec entirely (or narrow to just packsswb/packssdw until 269 fixes them, then delete). Remaining 266 ACs: (AC3) document the legacy-SSE preserve-upper rule centrally in conventions.md §semantics-traps (replacing the false fuzz comment); (AC4) the filter removal — both can ride with TASK-269 or a small doc commit.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
