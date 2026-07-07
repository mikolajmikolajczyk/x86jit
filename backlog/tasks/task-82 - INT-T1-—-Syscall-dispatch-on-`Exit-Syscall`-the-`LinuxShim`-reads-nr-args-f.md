---
id: TASK-82
title: >-
  INT-T1 — Syscall dispatch on `Exit::Syscall`: the `LinuxShim` reads nr + args
  f
status: Done
assignee: []
created_date: '2026-07-06 11:06'
updated_date: '2026-07-07 10:01'
labels:
  - 'crate:linux'
milestone: integration-native-diff
dependencies: []
ordinal: 82000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Syscall dispatch on `Exit::Syscall`: the `LinuxShim` reads nr + args from guest registers, forwards file ops (`open`/`read`/`close`) to the host kernel (via `std::fs`, read-only path allowlist), writes the result to RAX, resumes. x86-host-only in effect. (T§12, §1)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone integration-native-diff).
<!-- SECTION:FINAL_SUMMARY:END -->
