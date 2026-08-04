---
id: TASK-229
title: >-
 core: shrink x86jit-core to a standalone reproducer for the VFloatMov
 store-narrowing defect, then report upstream
status: To Do
assignee: []
created_date: '2026-07-29 11:20'
updated_date: '2026-07-29 17:58'
labels:
 - bug
 - toolchain
 - core
dependencies: []
ordinal: 325000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
TASK-223 is a wrong answer produced only by optimized builds of x86jit-core: with the lane mask const-folded, the 128-bit store in exec_v_float_mov kept its low half and dropped bits 127:64. It is worked around in the tree by a byte-wise merge, not fixed — nobody knows yet whether the defect is ours or the compiler's, and until that is settled the workaround is load-bearing and undocumented outside the task.

Established so far:
 * opt-level 0 correct, 1/2/3 wrong; debug-assertions irrelevant; package bisect points at x86jit-core
 * black_box on the mask fixes it; black_box on the loaded value or on the index does not
 * #[inline(never)] on exec_v_float_mov fixes it, so it is the inlined copy inside step_one
 * -C target-cpu native / x86-64-v2 make no difference
 * TASK-228 (real pre-existing guest-RAM UB) fixed first, changed nothing here
 * Miri reports no UB of ours on the executed path under Tree Borrows; the only residual Stacked
 Borrows complaint is inside iced-x86
 * a minimal replica (same enum + match + [u128; 32] + lane_mask + identical merge) is CORRECT at
 opt-level 3, so the trigger needs the real step_one context
 * rustc 1.96.1, LLVM 22.1.2, x86_64-unknown-linux-gnu
 * no matching known rust-lang/LLVM issue found

The work is to shrink from the real crate downward — keep step_one and delete unrelated arms/ops until the smallest thing that still miscompiles — rather than to build up from a minimal case, which has already been tried and does not reproduce. Then either file upstream with that reproducer, or find our own UB and fix it properly and revert the workaround.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A self-contained crate reproduces the wrong result at opt-level >= 1 and is correct at 0
- [ ] #2 The question is answered either way: an upstream issue is filed with the reproducer, OR our own defect is identified
- [ ] #3 If it turns out to be ours, exec_v_float_mov's byte-wise workaround is reverted to the clearer masked form
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AARCH64 IS NOT AFFECTED — measured 2026-07-29, and it changes the blast radius.

The same IR retargeted to aarch64-unknown-linux-gnu and run under qemu-aarch64 is correct on all
four variants; the backend emits ldp/stp for both halves, which is exactly what x86-64 fails to do.
Executed, not read off the disassembly, and with a POSITIVE CONTROL: a fifth module that zeroes the
high half in the IR itself does come back MISCOMPILED through the same harness, so the aarch64 leg
can detect a wrong answer rather than passing by default. The freestanding driver was first
validated against the libc one on x86-64 (same verdict) before being trusted on ARM.

Consequence for us: ARM is the primary target, so SHIPPED ARM BUILDS WERE NEVER WRONG. The defect
window was x86-host only — which is where the differential/oracle work runs and where the embedder
that reported it runs, hence the retail-title symptom.

run.sh now carries the aarch64 leg (skipped cleanly when clang/qemu-aarch64 are absent), and doc-35
records it. Also useful upstream whenever this is filed: it narrows the search to x86-64
legalization.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
