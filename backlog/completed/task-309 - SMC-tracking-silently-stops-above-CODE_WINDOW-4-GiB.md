---
id: TASK-309
title: SMC tracking silently stops above CODE_WINDOW (4 GiB)
status: Done
assignee: []
created_date: '2026-08-10 15:38'
updated_date: '2026-08-11 11:02'
labels: []
dependencies: []
priority: medium
ordinal: 345000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
memory.rs:287 — mark_code and note_write no-op for pages beyond the 4 GiB table, and the comment beside it acknowledges that code above the boundary, and blocks straddling it, are valid configurations.

So for guest code placed high, a write never dirties the page, the cache epoch never bumps, and a stale translation stays executable indefinitely. Nothing reports it. The assumption 'guest code always lives low' holds for the current fixture set and is not an architectural guarantee — a PS4 or Go guest is free to break it.

Either track the whole address space sparsely (WatchPages already does), or refuse to translate outside the tracked range so the limitation is loud.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Either SMC is tracked across the full address space, or translation outside it is refused
- [ ] #2 Tests cover code entirely above 4 GiB and a block straddling the boundary
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Merged into the multi-vcpu soundness task 2026-08-11. Same property, same test harness (a deterministic two-vcpu race), and fixing any one of them in isolation leaves the guarantee unstated. Each site kept its own acceptance criterion there.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
