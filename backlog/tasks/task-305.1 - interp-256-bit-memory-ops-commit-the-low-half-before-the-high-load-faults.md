---
id: TASK-305.1
title: 'interp: 256-bit memory ops commit the low half before the high load faults'
status: To Do
assignee: []
created_date: '2026-08-10 15:37'
labels: []
dependencies: []
parent_task_id: TASK-305
priority: high
ordinal: 338000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
interp/vector.rs (~3317): the low 128-bit result is stored to the destination, then the high memory half is loaded. If that load faults, rip is rewound but the destination keeps the partial result. With the legal dst==src1 aliasing, the retry reads its own partial output as the source: 'vaddps ymm0, ymm0, [mem]' double-adds the low lanes once the page is mapped.

The write-before-second-load shape repeats across the 256-bit logic, integer, conversion, shuffle and compare handlers, so this is a sweep, not a one-line fix.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 No 256-bit handler writes the destination before both memory halves have loaded
- [ ] #2 A native fault/retry test with dst==src1 fails against the current code
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
