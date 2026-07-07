---
id: TASK-74
title: 'M7-T2 — Cache synchronization: the shared `TranslationCache` fills under `RwLo'
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m7-multithreading-tso
dependencies: []
ordinal: 74000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Cache synchronization: the shared `TranslationCache` fills under `RwLock` — the first thread to miss a block lifts+inserts it; concurrent misses may lift redundantly but insert the same valid block (translate-*at-least*-once, always correct). The hot loop is then reused via cache hit (interp) or chained link (JIT) rather than re-lifted. (§9, §11)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m7-multithreading-tso).
<!-- SECTION:FINAL_SUMMARY:END -->
