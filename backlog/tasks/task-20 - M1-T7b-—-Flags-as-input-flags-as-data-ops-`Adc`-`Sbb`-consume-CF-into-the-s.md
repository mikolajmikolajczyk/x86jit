---
id: TASK-20
title: >-
  M1-T7b — Flags-as-input / flags-as-data ops: `Adc`/`Sbb` (consume CF into the
  s
status: Done
assignee: []
created_date: '2026-07-06 11:04'
labels: []
milestone: m1-ir-interpreter
dependencies: []
ordinal: 20000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Flags-as-input / flags-as-data ops: `Adc`/`Sbb` (consume CF into the sum) and `GetCond { dst, cond }` (materialize a condition as 0/1 for `setcc`/`cmovcc`/`rcl`/`rcr`). Without these you can't lift `adc`/`sbb`, which appear in every 128-bit add chain glibc/compilers emit. (§6.2, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
