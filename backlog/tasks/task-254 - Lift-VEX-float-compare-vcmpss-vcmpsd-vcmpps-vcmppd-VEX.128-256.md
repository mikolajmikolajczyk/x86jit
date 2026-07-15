---
id: TASK-254
title: Lift VEX float-compare vcmpss/vcmpsd/vcmpps/vcmppd (VEX.128/256)
status: Done
assignee: []
created_date: '2026-07-15 18:02'
updated_date: '2026-07-15 18:02'
labels:
  - lift
  - m8-simd
dependencies: []
ordinal: 284000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The VEX 3-operand float-compare-with-predicate family (`vcmp{ss,sd,ps,pd}`, VEX.128 and
VEX.256) was unlifted: e.g. `c5 ea c2 e0 01` = `vcmpltss xmm4,xmm2,xmm0` decoded to a
mnemonic no lift arm matched and faulted as UnknownInstruction. Unlike the legacy
2-operand `cmp{ss,sd,ps,pd}` (dst is also src1), the VEX form is 3-operand: `dst =
cmp(src1, src2/mem)` with src1 and dst distinct, and the imm8 predicate is the last
operand (op3, not op2). VEX.128 zeroes bits 255:128; VEX.256 fills them.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Vcmpss/Vcmpsd/Vcmpps/Vcmppd lifted (reg + mem src2), VEX.128 zeroing + VEX.256 fill; scalar merges upper lanes from src1
- [x] #2 All 8 legacy predicates plus the full 32-entry VEX/AVX extended set handled per the AVX table (no silent alias into the low 8)
- [x] #3 Differential (VEX.128 == legacy SSE via unicorn oracle) + jit==interp (128+256, reg+mem, scalar+packed, legacy+extended preds) + register-survival + exact-bytes regression
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. New `lift_vfloat_cmp_mask` dispatches: YMM dst → `VFloatCmpMask256`/`VFloatCmpMask256M`
(compare each 128-bit half); else VEX.128 reuses the SSE `VFloatCmpMask`/`VFloatCmpMaskM`
op with a distinct src1 `a` + trailing `VZeroUpper`. The scalar upper-lane merge base was
switched from `dst` to `a` (src1) in both the interp (`exec_v_float_cmp_mask`) and the JIT
(`merge_cmp_mask`) — identical for the 2-operand SSE form (a==dst), correct for the
3-operand VEX form. `float_pred` (interp) and `build_float_cmp_mask` (cranelift) both
extended from the low 8 (`pred & 7`) to the full 32 (`pred & 31`): the extra bits select
GE/GT/TRUE/FALSE and the signaling (_S) vs quiet (_Q) #IA-on-QNaN nuance we don't model, so
the 32 collapse to eight boolean outcomes keyed on the partial_cmp ordering — no silent
low-8 alias. Tests: vcmp_vex128_eq_sse (VEX.128 == unicorn-validated legacy SSE, all 8
preds, reg+mem, scalar+packed), vcmp_vex_match_interp (jit==interp, 128+256, reg+mem,
legacy + extended preds, dirtied ymm_hi proving VEX.128 zero / VEX.256 fill),
survival_vcmp (full sentinel register file), vcmpltss_exact_bytes_lifts (the exact failing
c5 ea c2 e0 01 bytes run to HLT with the right mask/merge/zeroing). Ratchet allowlist +
coverage regen (6 VEX cmp encodings now covered). clippy + fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
