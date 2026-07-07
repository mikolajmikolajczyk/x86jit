---
id: TASK-86
title: >-
  INT-T6 — The whole-program tests run a fixed-input binary and capture its
  deter
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:tests'
milestone: integration-native-diff
dependencies: []
ordinal: 86000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The whole-program tests run a fixed-input binary and capture its deterministic output (stdout bytes / exit code), not raw memory/registers. `tests/whole_program.rs`, `tests/busybox.rs`. (T§12.3)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone integration-native-diff).
<!-- SECTION:FINAL_SUMMARY:END -->
