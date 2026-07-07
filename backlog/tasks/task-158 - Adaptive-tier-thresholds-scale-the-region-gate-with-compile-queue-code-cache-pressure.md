---
id: TASK-158
title: >-
  Adaptive tier thresholds: scale the region gate with compile-queue /
  code-cache pressure
status: To Do
assignee: []
created_date: '2026-07-07 15:55'
labels:
  - 'crate:core'
  - 'goal:perf'
milestone: open-backlog
dependencies:
  - TASK-156
ordinal: 167000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Production JITs adapt thresholds to load: HotSpot scales CompileThreshold with compile-queue length and code-cache occupancy; under pressure it raises the bar so only the hottest code tiers up. After task-156 lands static T1/T2, make T2 (the region gate) adaptive: raise it when the compile queue is deep or the code cache is under pressure, lower it when idle. Prevents a burst of medium-hot loops from flooding the (heavy) region compiler.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 T2 rises with compile-queue depth / code-cache occupancy and relaxes when idle; no interp==JIT change
- [ ] #2 bench: a workload with many medium-hot loops does not thrash the region compiler (fewer speculative region compiles than static T2)
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Signals available: queue depth (cranelift Shared.queue.outstanding), code-cache size (cranelift-jit module bytes / a running counter). Expose a cheap read to the core dispatcher (a Backend method, decision-5 style — backend never touches the cache). Keep it a pure heuristic; document the curve. Depends on task-156's two-threshold structure. Optional: a code-cache eviction/quota once AOT (task-103) or long runs make cache size matter.
<!-- SECTION:PLAN:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
