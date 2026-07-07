---
id: TASK-46
title: >-
  M3-T5b — Seam-2 marker (§17.4): keep the cache key `u64` (guest addr) today,
  bu
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m3-translation-cache
dependencies: []
ordinal: 46000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Seam-2 marker (§17.4): keep the cache key `u64` (guest addr) today, but leave the `SEAM` comment noting it would become `BlockKey { guest_addr, mode }` if processor modes were ever added. Don't build `BlockKey` now. (§17.4, §17.6)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m3-translation-cache).
<!-- SECTION:FINAL_SUMMARY:END -->
