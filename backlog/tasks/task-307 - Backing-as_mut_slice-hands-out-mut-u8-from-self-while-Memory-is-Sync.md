---
id: TASK-307
title: 'Backing::as_mut_slice hands out &mut [u8] from &self while Memory is Sync'
status: To Do
assignee: []
created_date: '2026-08-10 15:38'
labels: []
dependencies: []
priority: high
ordinal: 343000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
memory.rs:170. Manufacturing an unrestricted &mut [u8] from &self, on a type that is manually Sync and shared across vcpus through Arc<Vm>, is Rust aliasing UB whenever two accesses overlap — which is the normal case for a guest with threads.

This is a different problem from guest memory ordering and is not fixed by TSO barriers: the barriers make the *guest's* model right, while this makes the *host* program ill-formed and lets LLVM miscompile it. It has not visibly broken anything, which is exactly how this class of bug behaves until a compiler upgrade.

Replace with raw-pointer copy primitives under a documented synchronisation model, or atomic cell storage. Validate under Miri.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 No &mut reference is created from a shared &self on guest RAM
- [ ] #2 The multi-vcpu tests run clean under Miri (or the reason they cannot is recorded)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
