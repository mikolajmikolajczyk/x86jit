---
id: TASK-319
title: 'INVESTIGATE: do stores from compiled code invalidate translated pages?'
status: Done
assignee: []
created_date: '2026-08-10 15:40'
updated_date: '2026-08-11 11:02'
labels: []
dependencies: []
priority: high
ordinal: 355000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
An adversarial review filed this as critical: 'JIT stores write host RAM directly and are not observed by handle_smc', so compiled code could patch compiled code and the old translation would keep running.

Not confirmed and not dismissed. Vm::unmap does invalidate (vm.rs:514, mirroring handle_smc, with a comment saying why), so the claim as stated is too broad — but the ordinary compiled-store path was not traced, and that is the one that matters.

Resolve it with a test rather than by reading: two vcpus, one patching a page the other has a compiled block on, then an immediate transfer into it. If the stale block runs, this becomes the highest-priority item in the backlog; if it does not, close it and record where the review's reading went wrong so the next reader does not repeat it. Touches TASK-217 (watch-bit inlining), which is in the same store path.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A two-vcpu compiled-code-patches-compiled-code test exists and its result is recorded
- [ ] #2 The finding is confirmed with a fix, or dismissed with the reason
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
