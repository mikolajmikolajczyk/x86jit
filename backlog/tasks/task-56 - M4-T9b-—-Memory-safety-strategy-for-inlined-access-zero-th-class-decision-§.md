---
id: TASK-56
title: >-
  M4-T9b — **Memory-safety strategy for inlined access (zero-th-class decision,
  §
status: Done
assignee: []
created_date: '2026-07-06 11:05'
labels: []
milestone: m4-jit-cranelift
dependencies: []
ordinal: 56000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
**Memory-safety strategy for inlined access (zero-th-class decision, §8.2.3).** Raw `host_base + guest_addr` with no check is host UB on any out-of-range guest address. Emit a bounds+permission check (recommended: a predictable branch to a slow-path stub returning `Exit::UnmappedMemory`/`MmioRead`/`MmioWrite`) — the *same* check routes Trap/MMIO out, so it does double duty with M4-T10. Guard pages are a later perf option. In `Flat`, addr 0 is valid → faithful null-`#PF` needs a per-page permission bitmap. (§8.2.3, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m4-jit-cranelift).
<!-- SECTION:FINAL_SUMMARY:END -->
