---
id: TASK-55
title: 'M4-T9 — Translate `IrOp`s one at a time, validating each against the interpret'
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:cranelift'
milestone: m4-jit-cranelift
dependencies: []
ordinal: 55000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Translate `IrOp`s one at a time, validating each against the interpreter: `InsnStart` (bake `guest_addr` as a const for the trapping accesses that follow → store to `cpu.rip` before an `Exit`), `ReadReg`/`WriteReg` (with upper-32 zeroing!), arithmetic/logic, flags in codegen (flag fields at stable `#[repr(C)]` offsets), `Load`/`Store` inlined (`host_base + guest_addr`, no callback), control-flow terminators. (§8.2.1, §8.2.3, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m4-jit-cranelift).
<!-- SECTION:FINAL_SUMMARY:END -->
