---
id: TASK-19
title: 'M1-T7 — Flag computation (Variant A, materialized) using **`FlagMask`, not `bo'
status: Done
assignee: []
created_date: '2026-07-06 11:04'
updated_date: '2026-07-07 10:02'
labels:
  - 'crate:core'
milestone: m1-ir-interpreter
dependencies: []
ordinal: 19000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Flag computation (Variant A, materialized) using **`FlagMask`, not `bool`** (§6.2): `inc`/`dec` keep CF; logic ops force CF=OF=0; shifts update flags **only when count ≠ 0** (runtime-conditional). iced says *which* flags; you encode *how*. (§3.2, §7, §16)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
