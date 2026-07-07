---
id: TASK-116
title: CR — fork under host-backed Reserved panics the host (should be a typed Exit)
status: Done
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:38'
labels:
  - 'crate:linux'
  - 'goal:harden'
milestone: code-review
dependencies: []
ordinal: 125000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
proc.rs spawn_child -> deep_copy panics for a host-backed Reserved memory. Latent (Reserved not wired to the loader yet). 'Go never forks' is wrong (os/exec forks via clone-without-CLONE_VM). Should surface as an Exit, not panic.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: Memory::deep_copy -> Option<Memory> (None for host-backed Reserved, was panic!); Vm::fork_with_backend -> Option<Vm>; proc.rs spawn_child -> Option<Process>; the deferred scheduler returns the guest -EAGAIN on None (fork's resource errno, matching the threaded-fork policy in deferred.md) instead of aborting the host. Flat/owned-Reserved fork unchanged. 72/72 core+linux+whole_program + 8/8 fork/pipe/shell green.
<!-- SECTION:NOTES:END -->
