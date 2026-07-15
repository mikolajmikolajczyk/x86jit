---
id: TASK-251
title: >-
  lift CMPSS — scalar-single compare F3 0F C2 /r ib with register + memory
  operand, all 8 predicates
status: Done
assignee: []
created_date: '2026-07-15 13:29'
updated_date: '2026-07-15 13:30'
labels: []
dependencies: []
ordinal: 281000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Lift CMPSS (F3 0F C2 /r ib) with a MEMORY second operand, which currently hard-faults the JIT/interpreter with UnknownInstruction (a real guest hit 'cmpneqss 0x24(%rsp),%xmm0', bytes f3 0f c2 44 24 24 04). The register-operand form of CMPSS/CMPSD/CMPPS/CMPPD was already lifted via VFloatCmpMask; lift_float_cmp_mask read the second operand with reg_xmm() only and bailed to unsupported_insn on a memory operand. CMPSS writes an all-ones/all-zeros 32-bit mask into the dest's low dword per the imm8 predicate (EQ/LT/LE/UNORD/NEQ/NLT/NLE/ORD = 0..7); the upper 96 bits of the dest are preserved (SSE, non-VEX). Since lift_float_cmp_mask now routes reg/mem for all four CMP-family mnemonics via vec_src_dispatch!, CMPSD/CMPPS/CMPPD gain the memory-operand form for free by the same code path. Added a VFloatCmpMaskM IR op (memory second source), an interp exec (exec_v_float_cmp_mask_m) and a Cranelift lowering (emit_v_float_cmp_mask_m) that share the mask-build/merge helpers with the register form. NaN/unordered semantics modeled via partial_cmp (unordered => None): ordered preds (EQ/LT/LE/ORD) false on a NaN, the N/UNORD forms true, matching the host CPU (validated against Unicorn incl. QNaN in either operand slot).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 CMPSS with a memory operand (F3 0F C2 /r ib) lifts and executes in both interpreter and Cranelift JIT tiers, all 8 imm8 predicates
- [x] #2 scalar form writes the low-dword mask and preserves the dest's upper 96 bits; NaN/unordered predicate semantics match the host CPU
- [x] #3 differential/Unicorn coverage: all 8 predicates x reg+mem operand, equal/less-than, NaN as lhs and as comparand; JIT-vs-interp register-survival case; full suite green
<!-- AC:END -->



## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
