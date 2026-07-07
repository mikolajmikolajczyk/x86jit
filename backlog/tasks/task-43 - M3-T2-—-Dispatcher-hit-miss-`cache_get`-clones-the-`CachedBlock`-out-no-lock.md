---
id: TASK-43
title: 'M3-T2 — Dispatcher hit/miss: `cache_get` clones the `CachedBlock` out (no lock'
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m3-translation-cache
dependencies: []
ordinal: 43000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Dispatcher hit/miss: `cache_get` clones the `CachedBlock` out (no lock guard held across execution — SMC safety); miss → lift → `materialize` → insert. (§9.2)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m3-translation-cache).
<!-- SECTION:FINAL_SUMMARY:END -->
