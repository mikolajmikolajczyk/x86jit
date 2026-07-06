---
id: TASK-67
title: 'M5-T5 — "Fires" axis: an `OptStats` counter per optimization (`chained_jumps`,'
status: Done
assignee: []
created_date: '2026-07-06 11:06'
labels: []
milestone: m5-performance
dependencies: []
ordinal: 67000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
"Fires" axis: an `OptStats` counter per optimization (`chained_jumps`, `elided_flag_calcs`, …) + a targeted test on a crafted input asserting the counter moved. Catches the silent no-op where the opt does nothing and passes correctness because "nothing changed = nothing broke". (T§8.2)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m5-performance).
<!-- SECTION:FINAL_SUMMARY:END -->
