---
id: TASK-105
title: M5-T3 — Superblocks / traces (profiling justified it). Delivered as an **opt-i
status: Done
assignee: []
created_date: '2026-07-06 11:07'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 105000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Superblocks / traces (profiling justified it). Delivered as an **opt-in** capability (`JitBackend::with_superblocks(caps)`) over 6 phases (T3a–f, see [`../design/superblock-plan.md`](../design/superblock-plan.md)): a fuel-based block budget in the ABI, region formation (`lift_region`, DAG + loops in reverse-post-order), a real Cranelift CFG (`translate_region`), multi-span SMC, and **SSA loop-carried GPRs** (registers Variables across the loop, flushed at every exit/trap) — a hot loop's execution runs **~3× faster** (SHA-256 18.1 → 6.3 ms warm, ~3× native). Kept opt-in, not default-on: the region compile is heavier and workload-dependent (default-on regresses CPython 90 s → 280 s), so it's a per-workload knob. **Follow-up for a safe default:** written-set flush + lower region opt-level. (§12 M5)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
