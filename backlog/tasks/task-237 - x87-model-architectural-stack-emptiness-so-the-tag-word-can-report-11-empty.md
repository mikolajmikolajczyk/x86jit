---
id: TASK-237
title: 'x87: model architectural stack-emptiness so the tag word can report 11 (empty)'
status: To Do
assignee: []
created_date: '2026-08-04 15:38'
labels:
  - x87
  - core
dependencies: []
ordinal: 333000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`tag_word` derives tags from the live `fpr[]` bytes, so it can express valid / zero / special but never `11` (empty). The register file has no emptiness state to derive it from — every slot always holds bytes.

Measured on a real CPU versus this engine:

    fninit; fnstenv        hardware 0xffff   engine 0x5555
    fninit; fld1; fnstenv  hardware 0x3fff   engine 0x1555

So a guest that reads the tag word to learn how many slots are occupied — which is the field's whole purpose — is told "all eight hold zero" instead of "all eight are empty". FreeBSD's fenv and MSVC's FPU-state helpers both read it.

The fix is architectural, not a patch in `tag_word`: an emptiness bit (or a tag pair) per physical register, maintained through push, pop, `fninit`, `fldenv`, `frstor` and `fxrstor`, with `tag_word` reading it instead of guessing from bytes. `exec_fxstate`'s abridged FTW makes the same simplification and would be fixed by the same state.

Pinned meanwhile by `x87_tag_word_after_fninit_diverges_from_hardware`, which asserts the divergent values deliberately so they cannot drift; that test must fail and be updated when this lands.

Raised by an adversarial review of the fnstenv work. The limitation was documented but under-stated, and the fnstenv test filled all eight registers, so it could not surface the empty case.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 CpuState carries per-register architectural tag/emptiness state, maintained across push, pop, fninit, fldenv, frstor and fxrstor
- [ ] #2 fninit; fnstenv writes 0xffff and fninit; fld1; fnstenv writes 0x3fff, matching hardware
- [ ] #3 exec_fxstate's abridged FTW is derived from the same state rather than from the raw bytes
- [ ] #4 the pinning test is updated to assert the hardware values instead of the divergent ones
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
