---
id: TASK-320
title: 'compat probe: instantiate memory forms, not only register forms'
status: To Do
assignee: []
created_date: '2026-08-10 19:29'
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

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
