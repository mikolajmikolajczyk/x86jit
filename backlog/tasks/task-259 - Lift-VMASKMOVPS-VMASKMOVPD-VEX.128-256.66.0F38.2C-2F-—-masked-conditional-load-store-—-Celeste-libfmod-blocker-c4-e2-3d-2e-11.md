---
id: TASK-259
title: >-
  Lift VMASKMOVPS/VMASKMOVPD (VEX.128/256.66.0F38.2C-2F) — masked conditional
  load/store — Celeste libfmod blocker c4 e2 3d 2e 11
status: Done
assignee: []
created_date: '2026-07-16 13:32'
updated_date: '2026-07-16 14:02'
labels:
  - celeste
  - avx
  - vex
dependencies: []
ordinal: 289000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Celeste (CUSA11302) faults in **libfmod** (FMOD audio) on a 256-bit masked-store AVX1 op that x86jit does not lift.

Concrete blocker bytes (identity-mapped guest VA 0xd5bc00, captured by unemups4's UnknownInstruction reporter):
    c4 e2 3d 2e 11            vmaskmovps ymmword ptr [rcx], ymm8, ymm2   (VEX.256.66.0F38.W0 2E /r)
Two more in the run of instructions right after it:
    c4 62 3d 2e 0b            vmaskmovps ymmword ptr [rbx], ymm8, ymm9
    c4 e2 55 2e 19            vmaskmovps ymmword ptr [rcx], ymm5, ymm3

This is the AVX1 register-mask conditional move family (NOT the existing EVEX-opmask IrOp::VMaskMov, which is k-register masked vmovdqu). Encoding: VEX.NDS.128/256.66.0F38.W0, opcodes:
  2C  vmaskmovps  ymm/xmm, ymm/xmm(mask), m256/m128   (masked LOAD)
  2D  vmaskmovpd  ymm/xmm, ymm/xmm(mask), m256/m128   (masked LOAD)
  2E  vmaskmovps  m256/m128, ymm/xmm(mask), ymm/xmm   (masked STORE)  <-- the observed one
  2F  vmaskmovpd  m256/m128, ymm/xmm(mask), ymm/xmm   (masked STORE)
Semantics: the MASK operand is a vector register; per 32-bit (ps) / 64-bit (pd) element, if the element's most-significant (sign) bit is set, the element is loaded from / stored to memory; otherwise a load writes 0 to that dest element and a store leaves that memory element unmodified (no fault, no write). Widths: XMM (VEX.128, 4xps / 2xpd) and YMM (VEX.256, 8xps / 4xpd). Faulting elements whose mask bit is 0 must NOT fault — glibc/FMOD use it for tail-masked SIMD.

Lift across all three tiers (decode/interp/cranelift), matching the project's element-masked helper style. A shared exec_vmaskmov helper (mask-sign-bit -> per-element load/store) keeps JIT == interp, as done for the other vector ops.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vmaskmovps STORE (opcode 2E) xmm+ymm lifted, all 3 tiers; the concrete blocker c4 e2 3d 2e 11 runs with no fault
- [x] #2 vmaskmovpd STORE (opcode 2F) xmm+ymm lifted
- [x] #3 vmaskmovps/pd LOAD (opcodes 2C/2D) xmm+ymm lifted; mask-0 elements produce 0 (load) and mask-0 memory is untouched (store), and mask-0 elements never fault
- [x] #4 Unicorn differential + native-oracle tests green for masked load & store, incl. a mask with mixed set/clear sign bits and a store whose masked-off lanes point just past a mapped page (must not fault); clippy -D warnings + fmt clean; compat map regenerated
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Reuse EVEX masked_load_run/masked_store_run: convert vector-reg element-MSBs to a k-style bitfield (vec_msb_mask), then delegate. New IR ops VVecMaskLoadMem/VVecMaskStoreMem (mask=vector reg, load zeroes masked-off lanes). Lift Vmaskmovps/pd (elem 4/8) distinguishing load (op0=reg) vs store (op0=mem). New cranelift helper vec_maskmov_mem (reads mask reg, builds km, calls run fns). Tests: jit==interp + native oracle incl. mixed mask + masked-off store past mapped page (no fault). Regen compat map.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented 2026-07-16. Reused EVEX masked_load_run/masked_store_run: new vec_msb_mask (interp/mod.rs) converts a vector reg's per-element sign bits to a k-style bitfield. New IR ops VVecMaskLoadMem/VVecMaskStoreMem (load zeroes masked-off lanes). Lift Vmaskmovps/pd (elem 4/8) in lift_vmaskmov, distinguishing store (op0=mem) vs load; mask via reg_vec (XMM or YMM — an initial reg_xmm bug made ymm masks silently unsupported, caught by the native oracle since jit==interp double-trapped identically). Cranelift vec_maskmov_mem_helper + emit_v_vecmask_{load,store}_mem. Tests: differential vmaskmovps_celeste_wild_bytes (exact bytes c4 e2 3d 2e 11), jit==interp avx1_vmaskmov_match_interp, native oracle native_vmaskmov_matches_interp incl mixed mask + ymm store past mapped page (masked-off lanes no fault). Full suite --features unicorn minus fuzz: 767 passed. clippy/fmt clean. compat probe doesn't synthesize vmaskmov forms so map unchanged. NOT yet committed to main.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
