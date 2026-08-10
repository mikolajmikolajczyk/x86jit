---
id: TASK-35
title: M2-T1 — Widen instruction coverage as needed to run a real `_start` path (what
status: Done
assignee: []
created_date: '2026-07-06 11:05'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:core'
milestone: m2-first-program
dependencies: []
ordinal: 35000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Widen instruction coverage as needed to run a real `_start` path (whatever the target hello-world binary touches: more mov/arith variants, `test`, `movzx`/`movsx`/`movsxd`, `cdqe`/`cqo`, `syscall`, stack ops). Add each via the M1 lowering helpers. (§12 M2, §10)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m2-first-program).
<!-- SECTION:FINAL_SUMMARY:END -->
