---
id: TASK-274
title: >-
  lift VEX vextracti128/vextractf128 memory-destination form (currently
  unsupported_insn)
status: To Do
assignee: []
created_date: '2026-07-19 22:37'
labels:
  - lifter
  - avx
  - jit
dependencies: []
ordinal: 304000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`lift_insn` (x86jit-core/src/lift/mod.rs:1861) handles `Vextracti128 | Vextractf128` only for a register destination: `reg_xmm(insn, 0).ok_or_else(|| unsupported_insn(insn))?  // mem dst deferred`. The memory-destination encoding (`vextracti128 [mem], ymm, imm8`) therefore exits with UnknownInstruction. The IR op and both tiers already exist — `IrOp::VExtractLaneWideM` is lifted for the EVEX forms (`lift_vextract_wide`, lift/vector.rs:797) and lowered by interp + Cranelift (`emit_v_extract_lane_wide_m`) — so this is a lift-side gap only: route the OpKind::Memory case to VExtractLaneWideM with num_lanes=1, the same way lift_vextract_wide already does. Found while auditing JIT guest-memory write paths for task-273 (a watch_dirty regression test could not use the VEX encoding and had to fall back to EVEX vextracti32x4). Note also the stale doc comment at lift/vector.rs:796 claiming 'memory dst deferred' for lift_vextract_wide, which in fact handles it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 vextracti128 [mem], ymm, imm8 and vextractf128 [mem], ymm, imm8 lift to IrOp::VExtractLaneWideM instead of returning unsupported_insn
- [ ] #2 A jit==interp test covers both mnemonics in the memory-destination form
- [ ] #3 The watch_dirty task-273 regression test uses the VEX vextracti128 encoding (no EVEX/v4 fallback needed)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
