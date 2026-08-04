---
id: TASK-230
title: 'lift: VEX vextractf128 — LN''s AVX float-fill loop faults the guest'
status: Done
assignee: []
created_date: '2026-08-02 16:01'
updated_date: '2026-08-02 18:45'
labels:
 - lift
 - avx
dependencies: []
ordinal: 326000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found bringing up Little Nightmares (CUSA05952, UE4) on unemups4. The title reaches its save-data flow and then faults:

 guest fault: UnknownInstruction at 0x3b0a7b0 (rip 0x3b0a78d)
 unimplemented lift in x86jit for: vextractf128 $0x1,%ymm1,-0x50(%rdx)
 faulting bytes: [c4 e3 7d 19 4a b0 01]

Shape around it: an AVX float-fill loop that broadcasts with vpermilps/vinsertf128 and then stores 32 bytes at a time as vextractf128 + vmovups pairs. So the low half goes out with vmovups and the high half needs vextractf128 with imm8=1 to a memory operand.

Reached only on the pad-driven path past the save flow — an undriven smoke run times out before it, which is why it has not shown up before.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vextractf128 with imm8 0 and 1 lifts for both the register and memory destination forms
- [x] #2 differential-tested against the existing oracle, with the encoding pinned by an llvm-mc witness
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented 2026-08-02, together with TASK-208 (same gap). Lift-side only: the Vextracti128|Vextractf128 arm in lift/mod.rs now routes an OpKind::Memory destination to lift_vextract_wide(..., num_lanes=1) -> IrOp::VExtractLaneWideM. The IR op and both tiers (interp + emit_v_extract_lane_wide_m) already existed, so no IR/interp/Cranelift change was needed. Stale 'memory dst deferred' doc comment on lift_vextract_wide (lift/vector.rs) fixed. Tests: jit.rs vextract128_mem_dst_match_interp (f128/i128 x imm8 0/1 x mem, plus reg-dst with pre-dirtied ymm_hi to observe the VEX upper-clear), native.rs native_vextract128_mem_dst_matches_interp (real CPU, avx2-gated, CONFIRMED to run rather than skip). Encodings pinned byte-for-byte with both objdump and llvm-mc, including the literal LN bytes c4 e3 7d 19 4a b0 01. compat/coverage unchanged: both mnemonics were already in the artifact and the ratchet ALLOWLIST. Gates on the merged main tree: 916 passed / 8 skipped (--features unicorn, minus fuzz_robustness), clippy clean, fmt clean, cargo check --target aarch64 clean. NOT COMMITTED - lives as uncommitted changes in the main working tree.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
