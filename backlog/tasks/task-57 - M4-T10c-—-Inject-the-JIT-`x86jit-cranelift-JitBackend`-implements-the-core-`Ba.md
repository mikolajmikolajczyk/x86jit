---
id: TASK-57
title: >-
  M4-T10c — Inject the JIT: `x86jit-cranelift::JitBackend` implements the core
  `Ba
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:cranelift'
  - 'crate:core'
milestone: m4-jit-cranelift
dependencies: []
ordinal: 57000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Inject the JIT: `x86jit-cranelift::JitBackend` implements the core `Backend` trait; the user builds the `Vm` via `Vm::with_backend(cfg, Box::new(JitBackend::new(..)))`. The core never names the JIT crate. `materialize(&self)` → compiler state behind a `Mutex`. (§4.1, §8)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m4-jit-cranelift).
<!-- SECTION:FINAL_SUMMARY:END -->
