---
id: TASK-354
title: 'Read X86JIT_NO_FLAG_ELISION once, not on every lifted instruction'
status: Done
assignee: []
created_date: '2026-09-24 11:27'
updated_date: '2026-09-24 11:54'
labels: []
milestone: ps4-perf
dependencies: []
priority: high
ordinal: 390000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
elide_dead_flags calls std::env::var_os("X86JIT_NO_FLAG_ELISION") every time it runs, i.e. on every lift. An embedder that single-steps (step_instruction lifts one instruction per step) pays a getenv per guest instruction, and on macOS getenv takes a process-wide lock, so threads stepping in parallel serialise on it. Measured in ps4-abi's caller sweep (6 threads): sample shows lift_one -> elide_dead_flags -> std::env::var_os -> getenv and _os_unfair_lock_lock_slow as the hot path, the largest modules taking minutes each. Fix: read the variable once, into a OnceLock, keeping its meaning (set before first use) unchanged.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The variable is read once per process and elide_dead_flags does no environment lookup on its hot path
- [x] #2 Behaviour with and without X86JIT_NO_FLAG_ELISION is unchanged
- [x] #3 The embedder's single-step cost is measured before and after
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Cache the env read in a OnceLock inside lift/mod.rs; measure with ps4-abi's probe before and after (single-thread and 6 threads) through a local [patch].
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
elide_dead_flags read X86JIT_NO_FLAG_ELISION with std::env::var_os on every lift. An embedder that single-steps (ps4-abi's probe-core) lifts once per guest instruction, and on macOS getenv takes a process-wide lock. sample of ps4-abi's 6-thread caller sweep showed lift_one -> elide_dead_flags -> var_os -> getenv and _os_unfair_lock_lock_slow as the hot path. The variable is now read once, into a OnceLock, and means what it did: set before first use.

Measured in ps4-abi, one caller image (libSceNpMatching2, 589 functions, 1068 call sites), single thread, --no-cache: 6.73 s user before, 4.95 s after (-26%). The gain under several threads is larger, since the lock contention goes too. Output compared field for field across the two builds: identical. x86jit-core tests: 136 passed.
<!-- SECTION:NOTES:END -->
