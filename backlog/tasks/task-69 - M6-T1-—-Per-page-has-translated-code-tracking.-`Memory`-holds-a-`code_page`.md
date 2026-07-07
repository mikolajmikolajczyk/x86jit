---
id: TASK-69
title: M6-T1 — Per-page "has translated code" tracking. `Memory` holds a `code_page`
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m6-smc
dependencies: []
ordinal: 69000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Per-page "has translated code" tracking. `Memory` holds a `code_page` bitmap (one atomic bool per `CODE_PAGE_BITS`=4 KiB page); `resolve()` calls `mem.mark_code(start, len)` when a block is cached. (§10)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m6-smc).
<!-- SECTION:FINAL_SUMMARY:END -->
