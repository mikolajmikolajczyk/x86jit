---
id: TASK-53
title: 'M4-T7 — `Backend::materialize` Jit arm: build a Cranelift `FunctionBuilder`, t'
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:cranelift'
milestone: m4-jit-cranelift
dependencies: []
ordinal: 53000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`Backend::materialize` Jit arm: build a Cranelift `FunctionBuilder`, translate `IrOp`s to a `Temp → cranelift Value` map (`Vec` sized `temp_count`), finalize into the arena → `CompiledPtr`. (§8.2.3)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m4-jit-cranelift).
<!-- SECTION:FINAL_SUMMARY:END -->
