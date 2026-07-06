---
id: TASK-106
title: >-
  FD-TIER — Hotness-gated tier-up (opt-in), delivered.
  `Vm::set_tier_up_after(Some
status: Done
assignee: []
created_date: '2026-07-06 11:07'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 106000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Hotness-gated tier-up (opt-in), delivered. `Vm::set_tier_up_after(Some(n))`: a block runs interpreted and is JIT-compiled only after `n` executions, so one-shot programs never pay compile cost for run-once blocks. Cuts the compile-bound one-shot penalty hugely with no hot-loop regression (`x86jit-bench experiment`, one host): sqlite 1095 → 43 ms (25×), lua 465 → 46 ms (10×), sha256 18 → 13 ms (1.4×), fib32 unchanged. Kept opt-in — default-on would erode JIT coverage (short differential/fuzz runs would never tier up, testing the interpreter instead). Complements FD-AOT (which attacks compile cost the other way, by persisting).
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
