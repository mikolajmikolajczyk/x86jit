---
id: TASK-320
title: 'compat probe: instantiate memory forms, not only register forms'
status: Done
assignee: []
created_date: '2026-08-10 19:29'
updated_date: '2026-08-11 11:03'
labels: []
dependencies: []
priority: medium
ordinal: 356000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-312 closed the publication problem by qualifying the map. The probe is still wrong: every *_or_mem / *_rm operand kind is instantiated as a register, so a Code counts as lifted when only its register encoding does, and memory-only shapes fall into the unencodable bucket.

Probe each legal operand alternative independently; a Code is fully lifted only when every form lifts, otherwise partial, naming the failing form. Expect the regenerated map to reveal gaps — triage them rather than treating the diff as noise.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Register and memory forms are probed separately
- [ ] #2 The regenerated map's new gaps are triaged into tasks
- [ ] #3 The generator's upper-bound caveat is removed once it is no longer true
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into the oracle blind spots task 2026-08-11. The compat probe and the fuzzer are the same axis — both only exercise register forms, so neither can reveal the other's gap — and the native capture is the third thing the 'cross-checked three ways' claim rests on. Fixing them apart leaves the claim qualified anyway.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
