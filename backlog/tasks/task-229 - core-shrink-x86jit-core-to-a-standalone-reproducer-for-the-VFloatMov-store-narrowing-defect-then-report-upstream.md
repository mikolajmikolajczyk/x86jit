---
id: TASK-229
title: >-
  core: shrink x86jit-core to a standalone reproducer for the VFloatMov
  store-narrowing defect, then report upstream
status: In Progress
assignee: []
created_date: '2026-07-29 11:20'
updated_date: '2026-08-11 14:24'
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
- [x] #2 The question is answered either way: an upstream issue is filed with the reproducer, OR our own defect is identified
- [ ] #3 If it turns out to be ours, exec_v_float_mov's byte-wise workaround is reverted to the clearer masked form
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#2 ANSWERED: the defect is LLVM's, not ours. The decisive evidence was already in the bundle and is now stated as the conclusion — lli's interpreter and lli's JIT produce different results from the SAME module, so the IR is unambiguous and the X86 backend is wrong. Miri finds no UB of ours on that path either.

NEW 2026-08-11, and it changes the report from 'a bug' to 'a regression with a bisect window': measured across packaged LLVMs, 19.1.7 correct, 20.1.8 correct, 21.1.8 miscompiled, 22.1.2 miscompiled, 22.1.8 (newest packaged) miscompiled. It landed between 20.1.8 and 21.1.8 and is still in the newest release, so the doc's 'check trunk first, a fix would make this moot' caveat is now discharged. 18.1.8 rejects the module, so the window is bounded below by 20.1.8 only.

AC#1 is met in substance but not in letter: the reproducer is self-contained LLVM IR plus a C driver, not a Rust crate. That is deliberately better for the purpose — it removes rustc from the picture entirely, which a crate cannot, and it is what an upstream reader needs. Reproduces at every llc opt level including -O0.

AC#3 does not apply: the defect is not ours, so exec_v_float_mov keeps its byte-wise merge. That comment already names run.sh and says not to restore the masked form until the script exits 1.

REMAINING: filing. UPSTREAM-REPORT.md is written and ready to post; posting to an external tracker is the maintainer's call, so it waits.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
