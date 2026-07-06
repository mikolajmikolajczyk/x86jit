---
id: TASK-72
title: 'M6-T4 — `tests/smc.rs`: the guest patches its own `.text` and re-executes the'
status: Done
assignee: []
created_date: '2026-07-06 11:06'
labels: []
milestone: m6-smc
dependencies: []
ordinal: 72000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`tests/smc.rs`: the guest patches its own `.text` and re-executes the new instruction (interpreter); an embedder rewrites a cached block via `write_bytes` and both backends re-lift it; a write to a data page does not invalidate. (§10, testing.md §6)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m6-smc).
<!-- SECTION:FINAL_SUMMARY:END -->
