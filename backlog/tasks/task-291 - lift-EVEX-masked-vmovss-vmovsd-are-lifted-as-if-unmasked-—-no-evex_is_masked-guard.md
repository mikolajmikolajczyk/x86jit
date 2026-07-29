---
id: TASK-291
title: >-
  lift: EVEX-masked vmovss/vmovsd are lifted as if unmasked — no evex_is_masked
  guard
status: To Do
assignee: []
created_date: '2026-07-29 09:54'
labels:
  - bug
  - avx512
  - lift
dependencies: []
ordinal: 321000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
lift_insn dispatches on iced's mnemonic, and Mnemonic::Vmovss/Vmovsd covers BOTH the VEX and EVEX encodings. lift_vscalar_fmove (x86jit-core/src/lift/vector.rs:3689) has no evex_is_masked(insn) rejection, unlike its ~10 sibling lifts in the same file. So an EVEX form carrying {k1}/{k1}{z} lifts to a plain VFloatMov and the opmask is silently dropped.

MEASURED at 564cb30, GuestCpuFeatures::v4(), k1 = 0 (all mask bits clear), so merge-masking must leave DEST completely untouched:

  62 f1 df 09 10 d6   vmovsd xmm2{k1},xmm4,xmm6
    xmm2 before = 44444444 33333333 22222222 11111111
    want        = 44444444 33333333 22222222 11111111   (mask bit 0 clear -> no write)
    got         = aaaaaaaa aaaaaaaa 00000000 3fb6cd8e

  62 f1 5e 09 10 d6   vmovss xmm2{k1},xmm4,xmm6
    got         = aaaaaaaa aaaaaaaa aaaaaaaa 3fb6cd8e

The EVEX memory forms are worse than the register form: the merge-masked load is 2-operand-with-mask, so the merge base is dropped as well.

Minimum viable fix is to reject the masked forms in the lift (Exit::UnknownInstruction) as the sibling lifts do; implementing the mask is a separate step. Found while investigating task-289.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A masked EVEX vmovss/vmovsd no longer lifts to an unmasked VFloatMov
- [ ] #2 The unmasked EVEX form and every VEX form keep working unchanged
- [ ] #3 A test pins both the register and memory EVEX masked forms with k1 = 0 and a destination that would visibly change if the mask were dropped
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
