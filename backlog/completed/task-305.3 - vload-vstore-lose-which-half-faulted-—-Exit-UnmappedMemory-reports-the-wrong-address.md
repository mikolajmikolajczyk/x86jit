---
id: TASK-305.3
title: >-
  vload/vstore lose which half faulted — Exit::UnmappedMemory reports the wrong
  address
status: Done
assignee: []
created_date: '2026-08-10 15:37'
updated_date: '2026-08-11 11:02'
labels: []
dependencies: []
parent_task_id: TASK-305
priority: high
ordinal: 340000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
interp/mod.rs (~4888): a 16-byte access is two 8-byte operations, but vload/vstore return only MemTrap, dropping whether addr or addr+8 failed. Callers then report the 16-byte base.

For an unaligned access whose first half is mapped and second is not, the embedder is told to map a page it has already mapped. It maps it, retries, faults identically, and loops. The wide-lane wrappers keep only the lane base and inherit the same defect.

An embedder cannot work around this: the information it needs was discarded before the Exit was built.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The faulting sub-address (and for writes, the failing chunk's width and value) reaches Exit::UnmappedMemory
- [ ] #2 A page-boundary test asserts the fault address and that a retry after mapping succeeds
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into the parent task-305 2026-08-11: one invariant, one contract, one place to state it. The site description and its acceptance criterion moved there verbatim.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
