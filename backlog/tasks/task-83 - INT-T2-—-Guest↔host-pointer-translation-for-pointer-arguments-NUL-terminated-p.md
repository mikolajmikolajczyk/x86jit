---
id: TASK-83
title: >-
  INT-T2 — Guest↔host pointer translation for pointer arguments: NUL-terminated
  p
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
milestone: integration-native-diff
dependencies: []
ordinal: 83000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Guest↔host pointer translation for pointer arguments: NUL-terminated path strings, `read`/`write` buffer copies between guest and host, and `writev` iovec-array gathering. (`host_base + guest_addr` is the flat-model translation.) `msghdr`/socket structs deferred (no networking program yet). (T§12)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone integration-native-diff).
<!-- SECTION:FINAL_SUMMARY:END -->
