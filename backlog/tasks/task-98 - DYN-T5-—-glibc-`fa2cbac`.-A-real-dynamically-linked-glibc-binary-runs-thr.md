---
id: TASK-98
title: >-
  DYN-T5 — **glibc** (`fa2cbac`). A real dynamically-linked glibc binary runs
  thr
status: Done
assignee: []
created_date: '2026-07-06 11:07'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 98000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
**glibc** (`fa2cbac`). A real dynamically-linked glibc binary runs three ways: `ld-linux` loads, file-backed-mmaps `libc.so.6`, resolves versioned symbols, and starts the program — all guest code. The version-resolution blocker was a chain of three shim bugs (fabricated `st_dev`/`st_ino` colliding with the main map's (0,0) in ld.so's dedup; missing `pslldq`; mmap reading `fd=-1` as a 64-bit value and misclassifying anonymous `MAP_FIXED` bss-zeroing as file-backed → stale `__exit_lock` → futex livelock). Groundwork from `143faea` (file-backed mmap, suffix redirect, SSE2 string ops, startup syscalls) plus a futex handler.
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
