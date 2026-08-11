---
id: TASK-304
title: 'BUG: lock adc / lock sbb lower to a non-atomic read-modify-write'
status: Done
assignee: []
created_date: '2026-08-10 15:37'
updated_date: '2026-08-11 11:01'
labels: []
dependencies: []
priority: high
ordinal: 336000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
rmw_of_binop (lift/mod.rs:2902) maps Add/Sub/And/Or/Xor to RmwOp and returns None for everything else, so LOCK-prefixed ADC and SBB fall through to a non-atomic load/compute/store while the mnemonic dispatch accepts them normally.

A guest using 'lock adc [mem], reg' for a carry-propagating multi-word counter — the reason the encoding exists — loses updates under contention with no diagnostic. The IR has no way to express a carry-dependent atomic RMW today, which is why it was left this way; silently weakening the guarantee is the part that is not acceptable.

Interim: return Unsupported for the LOCK forms so the guest traps instead of computing quietly wrong results. Proper: a CAS-loop IR op.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 lock adc / lock sbb are either atomic or trap; they never execute as a non-atomic RMW
- [ ] #2 A contended two-vcpu witness test would fail against the current behaviour
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into the 'Lifts that compute the wrong thing instead of trapping' task 2026-08-11 — both defects are the same failure mode and the same fix decision (trap, or represent it properly). Nothing dropped: the detail moved into that task's description and acceptance criteria.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
