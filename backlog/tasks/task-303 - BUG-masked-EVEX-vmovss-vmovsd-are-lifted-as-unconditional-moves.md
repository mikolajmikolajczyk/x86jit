---
id: TASK-303
title: 'BUG: masked EVEX vmovss/vmovsd are lifted as unconditional moves'
status: To Do
assignee: []
created_date: '2026-08-10 15:37'
labels: []
dependencies: []
priority: high
ordinal: 335000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
lift_vscalar_fmove (lift/vector.rs:3704) never consults evex_is_masked or op_mask — confirmed by grep, neither identifier appears in the function. Its memory paths always emit VStore/VLoad and its register path always emits VFloatMov.

So a masked EVEX VMOVSS/VMOVSD writes a destination the mask says to leave alone, and performs a memory access the mask says to suppress — including faulting on an address hardware would never touch. This is a wrong lift, which is strictly worse than an UnknownInstruction trap: the guest keeps running with corrupted state instead of stopping somewhere diagnosable.

Either reject evex_is_masked at the helper boundary until scalar mask semantics exist in the IR, or represent merge/zeroing and masked fault suppression properly. Rejecting is the safe interim step.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A masked EVEX vmovss/vmovsd either executes with correct merge/zeroing semantics or returns Unsupported
- [ ] #2 Tests cover k=0, merge, zeroing, and a masked-off load whose address is unmapped
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
