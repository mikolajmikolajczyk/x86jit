---
id: TASK-85
title: >-
  INT-T4 — The syscall set a static musl binary needs is covered:
  `open`/`openat`
status: Done
assignee: []
created_date: '2026-07-06 11:06'
labels: []
milestone: integration-native-diff
dependencies: []
ordinal: 85000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The syscall set a static musl binary needs is covered: `open`/`openat`, `read`, `write`, `writev`, `close`, `stat`/`fstat`, `brk`, `mmap`/`munmap`, `arch_prctl` (FS_BASE), `set_tid_address`, `rt_sigprocmask`, `ioctl`, `get/set uid/gid`, `exit`/`exit_group`. **Deferred until demanded:** `lseek`, `getrandom`. (T§12.5)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone integration-native-diff).
<!-- SECTION:FINAL_SUMMARY:END -->
