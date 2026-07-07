---
id: TASK-88
title: >-
  INT-T8 — Inputs are pinned (fixed argv + checked-in fixture files); the
  `Script
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:tests'
milestone: integration-native-diff
dependencies: []
ordinal: 88000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Inputs are pinned (fixed argv + checked-in fixture files); the `ScriptedSyscalls` responder exists for nondeterministic syscalls. No program run so far depends on ASLR/PID/time; an explicit quarantine check is unneeded until one does. (T§12.4)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone integration-native-diff).
<!-- SECTION:FINAL_SUMMARY:END -->
