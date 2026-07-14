---
id: TASK-247
title: lift SSSE3/AVX-128 integer horizontal add/sub family (vphaddd blocker)
status: In Progress
assignee: []
created_date: '2026-07-14 22:23'
updated_date: '2026-07-14 22:46'
labels:
  - lift
  - avx
  - ssse3
dependencies: []
ordinal: 276000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Mono managed/JIT'd code hits vphaddd %xmm0,%xmm0,%xmm0 (VEX.128.66.0F38 02 /r). Diagnosis: PHADDW/PHADDD/PHADDSW (0F38 01/02/03) and PHSUBW/PHSUBD/PHSUBSW (0F38 05/06/07), packed, are GENUINELY ABSENT — no dispatch, no IR op — for BOTH legacy SSSE3 and VEX. Add a shared VHInt/VHIntM IR op (operation enum {AddW,AddD,AddSW,SubW,SubD,SubSW}) mirroring the task-244 VHFloat/VHFloatM design, with interp + cranelift, and lift legacy (2-op in-place) + VEX.128 (3-op + upper-zero + 128-bit mem src). Semantics: horizontal pairwise add/sub of adjacent lanes within each source (w=16-bit, d=32-bit); the SW variants saturate signed words. Register + memory forms. Differential tests: legacy-vs-Unicorn, VEX via vex_eq_sse incl exact blocker vphaddd xmm0,xmm0,xmm0.
<!-- SECTION:DESCRIPTION:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Add VHInt/VHIntM IR op + HIntOp enum {AddW,AddD,AddSW,SubW,SubD,SubSW} mirroring task-244 VHFloat. Interp hint() compute (16/32-bit adjacent-pair add/sub, SW=signed-word saturate) + exec_v_h_int{,_m}. Cranelift emit + vhint/vhint_mem helpers (jit==interp). Lift legacy lift_hint (2-op in-place) + VEX lift_vhint (3-op, mem, upper-zero). Dispatch Phaddw/d/sw, Phsubw/d/sw + Vphaddw/d/sw, Vphsubw/d/sw. Tests + ALLOWLIST + compat.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Diagnosis: PHADDW/D/SW + PHSUBW/D/SW (0F38 01/02/03/05/06/07), packed, were GENUINELY ABSENT (no dispatch, no IR) for both SSSE3 and VEX. Added one shared VHInt/VHIntM IR op (HIntOp enum {AddW,AddD,AddSw,SubW,SubD,SubSw}) mirroring the task-244 VHFloat design. Interp hint() compute (adjacent-pair add/sub over 16/32-bit lanes; low half from a, high from b; Sw variants signed-saturate i16) + exec_v_h_int{,_m}; cranelift emit_v_h_int{,_m} via new vhint/vhint_mem helpers (jit==interp, hint_reg/hint_mem shared entry points). Lifted legacy lift_hint (2-op in-place, reg+128bit-mem) + VEX lift_vhint (3-op non-destructive reg + mem pre-copy, upper-zero). Semantics verified vs Intel SDM lane order. Covered 12 mnemonics: Phaddw/d/sw, Phsubw/d/sw + Vphaddw/d/sw, Vphsubw/d/sw. Tests: differential legacy-vs-Unicorn (reg+mem, incl saturation cases), VEX vex_eq_sse incl exact blocker vphaddd xmm0,xmm0,xmm0 + ymm-upper-zero, jit_eq_interp for all _m paths. Full suite 502 passed/3 skipped (--features unicorn, minus fuzz_robustness); clippy+fmt clean; 12 mnemonics added to ALLOWLIST + compat regen. No skips — whole PHADD/PHSUB family covered.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
