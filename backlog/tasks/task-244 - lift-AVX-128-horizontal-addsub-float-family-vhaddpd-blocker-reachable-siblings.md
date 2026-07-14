---
id: TASK-244
title: >-
  lift AVX-128 horizontal/addsub float family (vhaddpd blocker) + reachable
  siblings
status: In Progress
assignee: []
created_date: '2026-07-14 21:31'
updated_date: '2026-07-14 21:51'
labels:
  - lift
  - avx
  - sse3
dependencies: []
ordinal: 273000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 Mono/MonoGame bring-up hits vhaddpd %xmm0,%xmm0,%xmm0 (VEX.128.66.0F 7C /r). Diagnosis: HADDPD/PS, HSUBPD/PS (0F 7C/7D) and ADDSUBPD/PS (0F D0) are GENUINELY ABSENT — no dispatch, no IR op — for BOTH legacy SSE and VEX. Add a shared VHFloat/VHFloatM IR op (operation enum HAdd/HSub/AddSub + prec) mirroring VFloatBin/VFloatBinM, with interp + cranelift, and lift both legacy (2-operand in-place) and VEX.128 (3-operand + upper-zero + mem src) forms. Also assess adjacent VEX float ops whose SSE lifts but VEX/mem is missing: DPPD/VDPPS/VDPPD (VDpps IR exists), MOVDDUP/MOVSLDUP/MOVSHDUP, BLENDPS/PD imm. Reuse existing IR where a legacy SSE op already lifts; only add genuinely-missing ops. Register+128-bit-mem forms, VEX.128 upper-zeroing, differential tests (legacy-vs-Unicorn, VEX via vex_eq_sse, incl exact blocker vhaddpd xmm0,xmm0,xmm0).
<!-- SECTION:DESCRIPTION:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Core: add VHFloat{,M} IR op with HFloatOp enum {HAdd,HSub,AddSub} + prec, mirroring VFloatBin{,M}. Interp exec_v_h_float{,_m} + hfloat() compute (reuse apply_f32/apply_f64). Cranelift emit + helper. Lift: legacy lift_hfloat (2-op in-place) + VEX lift_vhfloat (3-op, mem, upper-zero). Dispatch Haddpd/ps/Hsubpd/ps/Addsubpd/ps + Vhaddpd/ps/Vhsubpd/ps/Vaddsubpd/ps. Siblings assessed separately: movddup family + dppd/vdpps + blendp imm — reuse where op exists, note skips. Tests + ratchet + compat.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Diagnosis: HADD/HSUB (0F 7C/7D) + ADDSUB (0F D0), packed PD/PS, were GENUINELY ABSENT (no dispatch, no IR) for both SSE and VEX. Added one shared VHFloat/VHFloatM IR op (HFloatOp enum {HAdd,HSub,AddSub} + prec) mirroring VFloatBin/VFloatBinM. Interp hfloat() compute + exec_v_h_float{,_m}; cranelift emit_v_h_float{,_m} via new vhfloat/vhfloat_mem helpers (jit==interp, hfloat_reg/hfloat_mem shared entry points). Lifted legacy (2-op in-place, reg+128bit-mem) via lift_hfloat and VEX.128 (3-op, non-destructive reg + mem pre-copy, upper-zero) via lift_vhfloat. Semantics verified against Intel SDM lane defs. Covered: Haddps/pd, Hsubps/pd, Addsubps/pd + Vhaddps/pd, Vhsubps/pd, Vaddsubps/pd (12 mnemonics). Tests: differential legacy-vs-Unicorn (reg+mem, both prec), VEX vex_eq_sse incl exact blocker vhaddpd xmm0,xmm0,xmm0 + ymm-upper-zero, jit_eq_interp for all _m paths. Full suite 493 passed/3 skipped (--features unicorn, minus fuzz_robustness); clippy+fmt clean; 12 mnemonics added to coverage ALLOWLIST + compat regen. DEFERRED to task-245 (all need new IR, none is the blocker): MOVDDUP/MOVSLDUP/MOVSHDUP, BLENDPS/PD imm-blend, DPPD + VDPPS/VDPPD (VDpps IR is f32-only).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
