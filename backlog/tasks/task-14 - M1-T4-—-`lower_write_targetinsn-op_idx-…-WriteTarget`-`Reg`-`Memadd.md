---
id: TASK-14
title: 'M1-T4 — `lower_write_target(insn, op_idx, …) -> WriteTarget` (`Reg` | `Mem{add'
status: Done
assignee: []
created_date: '2026-07-06 11:04'
labels: []
milestone: m1-ir-interpreter
dependencies: []
ordinal: 14000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`lower_write_target(insn, op_idx, …) -> WriteTarget` (`Reg` | `Mem{addr,size}`); for RMW compute the effective address **once** and reuse for Load + Store. (§7.1, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
