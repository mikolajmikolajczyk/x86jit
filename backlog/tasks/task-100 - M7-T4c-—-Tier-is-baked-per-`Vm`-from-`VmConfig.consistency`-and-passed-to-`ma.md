---
id: TASK-100
title: >-
  M7-T4c — Tier is baked per `Vm` (from `VmConfig.consistency`) and passed to
  `ma
status: Done
assignee: []
created_date: '2026-07-06 11:07'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
  - 'crate:cranelift'
milestone: open-backlog
dependencies: []
ordinal: 100000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tier is baked per `Vm` (from `VmConfig.consistency`) and passed to `materialize`; the cache is **not** keyed by tier. There's no runtime tier-switch API yet, so no flush path is needed; add one (flushing the whole cache) if/when a switch is exposed. (§8.2.3)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone open-backlog).
<!-- SECTION:FINAL_SUMMARY:END -->
