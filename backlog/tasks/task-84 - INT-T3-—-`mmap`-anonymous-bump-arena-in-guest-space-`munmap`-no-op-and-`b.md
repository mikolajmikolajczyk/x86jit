---
id: TASK-84
title: >-
  INT-T3 — `mmap` (anonymous bump arena in guest space), `munmap` (no-op), and
  `b
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
milestone: integration-native-diff
dependencies: []
ordinal: 84000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`mmap` (anonymous bump arena in guest space), `munmap` (no-op), and `brk` place results inside the guest address space. **Deferred:** `mprotect`, `MAP_FIXED`, file-backed `mmap`, and SoftMmu/W^X interaction — not needed by the static flat-model programs run so far. (T§12.3, §4.1, §9.1)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone integration-native-diff).
<!-- SECTION:FINAL_SUMMARY:END -->
