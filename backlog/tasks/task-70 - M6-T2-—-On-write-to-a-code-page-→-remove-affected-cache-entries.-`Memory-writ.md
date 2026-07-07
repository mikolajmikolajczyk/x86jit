---
id: TASK-70
title: 'M6-T2 — On write to a code page → remove affected cache entries. `Memory::writ'
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m6-smc
dependencies: []
ordinal: 70000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
On write to a code page → remove affected cache entries. `Memory::write`/`write_bytes` call `note_write`, which records dirtied code pages; the dispatcher drains them via `Vm::handle_smc`, and `TranslationCache::invalidate_overlapping` drops every block whose guest span overlaps the page. *JIT-side "mark host code dead" (freeing arena code + patching chained link slots) remains deferred — see below.* (§10, §9.1)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m6-smc).
<!-- SECTION:FINAL_SUMMARY:END -->
