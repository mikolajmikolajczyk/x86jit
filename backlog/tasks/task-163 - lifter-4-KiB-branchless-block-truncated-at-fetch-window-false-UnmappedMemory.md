---
id: TASK-163
title: >-
  lifter: >4 KiB branchless block truncated at fetch window -> false
  UnmappedMemory
status: Done
assignee: []
created_date: '2026-07-07 18:12'
updated_date: '2026-07-07 19:08'
labels:
  - 'crate:core'
  - go-caddy
  - 'goal:fix'
dependencies: []
ordinal: 172000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
lift_block capped its decode window at 4096 bytes; a branchless basic block longer than that (Go bignum crypto p521Square has >4 KiB adc/mul stretches) truncated its final instruction at the boundary, which iced flagged invalid -> spurious Exit::UnmappedMemory{access:Execute}. Fix: detect iced DecoderError::NoMoreBytes at a full (max_len-capped) window and cut the block cleanly at the last complete instruction, falling through to a continuation block. Discovered via go-caddy P5-real (task-153/161).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 lift_block cuts an over-long branchless block at a complete-instruction boundary and falls through instead of faulting
- [ ] #2 flags stay correct across the cut (carry chain), interp==unicorn
- [ ] #3 regression test with a >4 KiB branchless block
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented + tested (uncommitted, awaiting commit approval). x86jit-core/src/lift.rs: BLOCK_FETCH_WINDOW const; lift_block breaks cleanly on NoMoreBytes at a full window. Regression: differential::branchless_block_longer_than_fetch_window. Full non-fuzz suite 309/309 green (unicorn), clippy + fmt clean. Unblocks caddy under JIT (task-153): 'caddy version' now reaches exit 0 under JIT; interp still hits the MT-concurrency corruption in task-161.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
