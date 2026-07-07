---
id: TASK-39
title: M2-T5 — `programs/` test category; add `hello_static.elf` (checked-in fixture)
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:tests'
milestone: m2-first-program
dependencies: []
ordinal: 39000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`programs/` test category; add `hello_static.elf` (checked-in fixture). **Use a nolibc / freestanding binary** (raw `write`/`exit` via `syscall`, `-nostdlib`), NOT a static-glibc one — glibc's `__libc_start_main` calls SSE2 `memcpy`/`strlen` immediately, so a glibc hello secretly needs M8 (SIMD) before it prints. (T§3, T§11 M2, §12 M2, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m2-first-program).
<!-- SECTION:FINAL_SUMMARY:END -->
