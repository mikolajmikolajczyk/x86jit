---
id: TASK-295
title: >-
  core: shrink x86jit-core to a standalone reproducer for the VFloatMov
  store-narrowing defect, then report upstream
status: To Do
assignee: []
created_date: '2026-07-29 11:20'
labels:
  - bug
  - toolchain
  - core
dependencies: []
ordinal: 325000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
TASK-289 is a wrong answer produced only by optimized builds of x86jit-core: with the lane mask const-folded, the 128-bit store in exec_v_float_mov kept its low half and dropped bits 127:64. It is worked around in the tree by a byte-wise merge, not fixed — nobody knows yet whether the defect is ours or the compiler's, and until that is settled the workaround is load-bearing and undocumented outside the task.

Established so far:
  * opt-level 0 correct, 1/2/3 wrong; debug-assertions irrelevant; package bisect points at x86jit-core
  * black_box on the mask fixes it; black_box on the loaded value or on the index does not
  * #[inline(never)] on exec_v_float_mov fixes it, so it is the inlined copy inside step_one
  * -C target-cpu native / x86-64-v2 make no difference
  * TASK-294 (real pre-existing guest-RAM UB) fixed first, changed nothing here
  * Miri reports no UB of ours on the executed path under Tree Borrows; the only residual Stacked
    Borrows complaint is inside iced-x86
  * a minimal replica (same enum + match + [u128; 32] + lane_mask + identical merge) is CORRECT at
    opt-level 3, so the trigger needs the real step_one context
  * rustc 1.96.1 (31fca3adb), LLVM 22.1.2, x86_64-unknown-linux-gnu
  * no matching known rust-lang/LLVM issue found

The work is to shrink from the real crate downward — keep step_one and delete unrelated arms/ops until the smallest thing that still miscompiles — rather than to build up from a minimal case, which has already been tried and does not reproduce. Then either file upstream with that reproducer, or find our own UB and fix it properly and revert the workaround.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A self-contained crate reproduces the wrong result at opt-level >= 1 and is correct at 0
- [ ] #2 The question is answered either way: an upstream issue is filed with the reproducer, OR our own defect is identified
- [ ] #3 If it turns out to be ours, exec_v_float_mov's byte-wise workaround is reverted to the clearer masked form
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
