---
id: TASK-95
title: >-
  DYN-T2 — Shim honors `mmap` `MAP_FIXED` (returns the requested address — the
  fl
status: Done
assignee: []
created_date: '2026-07-06 11:06'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 95000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Shim honors `mmap` `MAP_FIXED` (returns the requested address — the flat region is already RW) and no-ops `mprotect`/`munmap`. The loader maps each object's full page span. *File-backed runtime `.so` mmap isn't needed for musl (its interpreter is pre-mapped); glibc will need it — see below.* (§4.1, §9.1)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
