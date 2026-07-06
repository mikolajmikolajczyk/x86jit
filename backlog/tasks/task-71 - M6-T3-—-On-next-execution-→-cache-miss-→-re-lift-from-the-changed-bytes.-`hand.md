---
id: TASK-71
title: M6-T3 — On next execution → cache miss → re-lift from the changed bytes. `hand
status: Done
assignee: []
created_date: '2026-07-06 11:06'
labels: []
milestone: m6-smc
dependencies: []
ordinal: 71000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
On next execution → cache miss → re-lift from the changed bytes. `handle_smc` runs at the top of the dispatch loop, before `resolve()`, so the next fetch re-lifts. (§10)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m6-smc).
<!-- SECTION:FINAL_SUMMARY:END -->
