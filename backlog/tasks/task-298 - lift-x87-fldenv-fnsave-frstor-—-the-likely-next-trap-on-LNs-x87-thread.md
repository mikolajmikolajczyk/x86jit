---
id: TASK-298
title: 'lift: x87 fldenv/fnsave/frstor — the likely next trap on LN''s x87 thread'
status: To Do
assignee: []
created_date: '2026-08-02 18:42'
labels:
  - lift
  - x87
dependencies: []
ordinal: 328000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Follow-up to TASK-297 (fnstenv). FreeBSD's fenv.h — which PS4 titles link — always pairs fnstenv with fldenv: fegetenv does fnstenv then fldcw, and feclearexcept/fesetenv/feupdateenv use fldenv proper. TASK-297 deliberately left fldenv, fnsave and frstor UNLIFTED so that a guest round-tripping the x87 environment traps loudly rather than restoring the partly-fabricated image fnstenv writes (FIP/CS+FOP/FDP/FDS are not modeled). That is the right call, but it means Little Nightmares is expected to fault again on the same thread as soon as it hits the restore half of the pair.

Doing this properly means deciding what fldenv should do with the environment fields we do not model. Options worth weighing before writing code: ignore the pointer block on load (consistent with never having produced a real one), or model FIP/FDP for real — the latter needs the last x87 opcode, which is not available at the helper, and note that modern CPUs only update FDP when an unmasked FP exception is pending (CPUID.07H:EBX[6], FDP_EXCPTN_ONLY) and we never raise one.

Also worth settling here: the status-word exception flags (C0-C3, PE/UE/OE/ZE/DE/IE) that TASK-297 names as a pre-existing model gap. fldenv loading flags we never set, and fnstenv storing flags we never computed, are the same gap seen from both ends.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 fldenv lifts, with an explicit decision recorded for the environment fields x86jit does not model
- [ ] #2 fnsave/frstor either lift or stay refused deliberately, with the reason stated rather than left implicit
- [ ] #3 differential-tested against Unicorn, with any deliberate divergence measured and named the way TASK-297 did
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
