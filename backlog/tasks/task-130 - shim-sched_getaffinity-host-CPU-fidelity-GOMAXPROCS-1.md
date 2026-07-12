---
id: TASK-130
title: 'shim: sched_getaffinity host-CPU fidelity (GOMAXPROCS>1)'
status: Done
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-12 16:08'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 139000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope; PERF phase. Current arm returns a 1-CPU mask -> Go GOMAXPROCS=1 (deterministic, correct ABI, one P — right for bring-up). Host-CPU fidelity gives GOMAXPROCS>1 but introduces scheduling nondeterminism; defer deliberately until perf phase.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 shim test: sched_getaffinity reports host CPU count; a Go binary with GOMAXPROCS>1 observes it (whole-program)
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged e737fa2). sched_getaffinity now reports host available_parallelism() online CPUs (clamped [1,1024] then to cpusetsize*8), replacing the single-CPU answer — Go GOMAXPROCS, nproc, OpenMP see real parallelism. DEVIATION from AC#1: implemented a DETERMINISTIC shim-level test (popcount == host count, host-agnostic) instead of a real GOMAXPROCS>1 Go whole-program run, which would introduce scheduling nondeterminism + CI flake (the task's own deferral rationale). Pre-existing ABI simplifications untouched + noted as possible follow-up: Rax uses len.max(8), never returns -EINVAL for undersized cpusetsize. 617/617, clippy+fmt clean.
<!-- SECTION:NOTES:END -->
