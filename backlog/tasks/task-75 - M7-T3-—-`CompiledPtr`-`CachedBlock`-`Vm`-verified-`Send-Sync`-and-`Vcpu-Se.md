---
id: TASK-75
title: 'M7-T3 — `CompiledPtr`/`CachedBlock`/`Vm` verified `Send + Sync` (and `Vcpu: Se'
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
  - 'crate:tests'
milestone: m7-multithreading-tso
dependencies: []
ordinal: 75000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`CompiledPtr`/`CachedBlock`/`Vm` verified `Send + Sync` (and `Vcpu: Send`) by a compile-time assertion in `tests/threads.rs` — the M4 wrapper the M7 trap depends on. (§9.1, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m7-multithreading-tso).
<!-- SECTION:FINAL_SUMMARY:END -->
