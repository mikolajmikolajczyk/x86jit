---
id: TASK-73
title: M7-T1 — Run multiple `Vcpu`s on separate host threads over one `Arc<Vm>`. `Vm`
status: Done
assignee: []
created_date: '2026-07-06 11:06'
labels: []
milestone: m7-multithreading-tso
dependencies: []
ordinal: 73000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Run multiple `Vcpu`s on separate host threads over one `Arc<Vm>`. `Vm` is structurally `Send + Sync` (shared `Memory` + `RwLock` cache + `Send + Sync` backend); each thread owns its `Vcpu` and `run()` loop. `tests/threads.rs::parallel_squares_*` runs 8 vcpus over one `Arc<Vm>`, both backends. (§2, §11)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m7-multithreading-tso).
<!-- SECTION:FINAL_SUMMARY:END -->
