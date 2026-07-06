---
id: TASK-102
title: >-
  M4-T10 — MMIO / trap in the JIT. The JIT bakes the guest's `Trap`-region
  window
status: Done
assignee: []
created_date: '2026-07-06 11:07'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 102000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
MMIO / trap in the JIT. The JIT bakes the guest's `Trap`-region window `[lo,hi)` (from `Memory::trap_window`, threaded through `materialize`) as a compile-time constant; an inlined load/store whose address lands in it returns `RET_MMIO_DEFER` with RIP on the faulting instruction and nothing committed. The dispatcher single-steps that one instruction on the interpreter (`interp::step_one` + `lift::lift_one`), which produces `Exit::MmioRead`/`MmioWrite` and, on resume, consumes the pending value (`complete_mmio_read`) or write-ack (`complete_mmio_write`). No per-access cost when the VM has no Trap regions (`trap_window` is `None` → no check emitted); mapping a Trap region invalidates the cache so stale check-less blocks recompile. Interp path also gained the symmetric write-resume. Differential `interp == JIT` covered by `smc::mmio_{read,write}_resumes_on_jit`. (§5.2, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
