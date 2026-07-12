---
id: TASK-190
title: >-
  Lift the remaining SSE2 packed-integer ops (saturating add/sub, pavg, pmul*,
  signed packs) — fuzzer found unlifted
status: Done
assignee: []
created_date: '2026-07-09 13:29'
updated_date: '2026-07-12 11:51'
labels:
  - m8-simd
dependencies: []
ordinal: 214000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The differential fuzzer (task-185) found these SSE2 packed-integer ops are NOT lifted (interp returns UnknownInstruction, real hardware runs them): packsswb, packssdw (signed byte/word packs; only packuswb is lifted), pmuludq, pmaddwd, pmulhw, pmullw (packed multiplies), pavgb, pavgw (packed average), paddusb/paddusw/psubusb/psubusw (unsigned saturating add/sub), paddsb/paddsw/psubsb/psubsw (signed saturating add/sub). Lift them (interp + codegen; several map to Cranelift's saturating/avg/mul packed ops). Then add them back to the fuzzer's VBin op list (currently capped at the 29 lifted ops). Low-priority but real SSE2 coverage gaps that real programs (image/audio codecs) hit.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 jit_eq_interp differential snippet per newly lifted op family (saturating add/sub, pavg, pmul*, signed packs)
- [ ] #2 fuzzer V_BIN_OPS menu re-extended with the new ops (task-185 restriction lifted)
- [ ] #3 native_matches_interp still green — real-CPU oracle validates the new SSE2 semantics
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (commit e97e83e). Lifted 13 SSE2 packed ops: paddsb/w, paddusb/w, psubsb/w, psubusb/w (sat add/sub via PackedBinOp AddSatS/U+SubSatS/U -> Cranelift sadd_sat/uadd_sat/ssub_sat/usub_sat), pavgb/w (AvgU -> avg_round), packsswb/packssdw (signed packs via VPackWide helper, signed=true), pmaddwd (dedicated helper->interp for aarch64 safety). pmullw/pmulhw/pmuludq already done in 215. All 13 added to the fuzzer VBin menu (oracled vs Unicorn) + FUZZER_COVERED; compat regen. Code-review clean (pmaddwd signed+i32-wrap, signed-pack lane order, sat lane widths, EVEX opcode map 15-19 encode==decode all verified). x86 598/598. ARM lowering of sat/avg confirmed via CI (below).
<!-- SECTION:NOTES:END -->
