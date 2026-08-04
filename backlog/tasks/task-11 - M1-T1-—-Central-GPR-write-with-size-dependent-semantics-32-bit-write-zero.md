---
id: TASK-11
title: 'M1-T1 — Central GPR write with **size-dependent semantics**: 32-bit write zero'
status: Done
assignee: []
created_date: '2026-07-06 11:04'
updated_date: '2026-07-07 10:02'
labels:
  - 'crate:core'
milestone: m1-ir-interpreter
dependencies: []
ordinal: 11000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Central GPR write with **size-dependent semantics**: 32-bit write zeroes upper 32 bits; 16/8-bit writes preserve them. One place, used by `WriteReg` interpretation and (later) codegen. (§7.1, §16 — the #1 silent bug)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
