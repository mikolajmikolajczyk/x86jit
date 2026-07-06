---
id: TASK-50
title: 'M4-T4 — Executable code arena (`memmap2`, W^X; macOS `pthread_jit_write_protec'
status: Done
assignee: []
created_date: '2026-07-06 11:05'
labels: []
milestone: m4-jit-cranelift
dependencies: []
ordinal: 50000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Executable code arena (`memmap2`, W^X; macOS `pthread_jit_write_protect`), owned by `Vm`, lifetime ≥ cache. `CompiledPtr` borrows into it. (§9.1)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m4-jit-cranelift).
<!-- SECTION:FINAL_SUMMARY:END -->
