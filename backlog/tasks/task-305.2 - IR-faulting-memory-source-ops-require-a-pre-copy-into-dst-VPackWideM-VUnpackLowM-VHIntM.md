---
id: TASK-305.2
title: >-
  IR: faulting memory-source ops require a pre-copy into dst (VPackWideM,
  VUnpackLowM, VHIntM)
status: To Do
assignee: []
created_date: '2026-08-10 15:37'
labels: []
dependencies: []
parent_task_id: TASK-305
priority: high
ordinal: 339000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
ir.rs (~681): VPackWideM omits its first source and documents that the lifter must copy it into dst before the faulting memory operation. That makes the destination dirty before the load that can fault. VUnpackLowM and VHIntM repeat it. VHFloatM already carries the source explicitly and shows the shape that works.

This is a representation defect, not an execution one — fixing the interpreter without fixing the op leaves the next backend free to reintroduce it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every faulting memory-source vector op carries its first source explicitly
- [ ] #2 VHFloatM's shape is the documented pattern for new ops of this kind
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
