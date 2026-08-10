---
id: TASK-22
title: 'M1-T8 — `Memory::read`/`write` scalar with **`write(&self)`** (interior mutabi'
status: Done
assignee: []
created_date: '2026-07-06 11:04'
updated_date: '2026-07-07 10:02'
labels:
  - 'crate:core'
milestone: m1-ir-interpreter
dependencies: []
ordinal: 22000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`Memory::read`/`write` scalar with **`write(&self)`** (interior mutability, `UnsafeCell` + `unsafe impl Sync`) — NOT `&mut self`. Guest RAM is shared across vcpus and written concurrently; `&mut` can't model M7 and forces a signature rewrite. Bounds-check every access → RAM value or `MemTrap` (never panic/UB). (§8 pitfall, §8.1)
<!-- SECTION:DESCRIPTION:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Delivered pre-migration (imported from the pre-migration milestone m1-ir-interpreter).
<!-- SECTION:FINAL_SUMMARY:END -->
