---
id: TASK-170.2
title: 'SIMD: width-parameterize vector ops (collapse 256/512 name-variants)'
status: To Do
assignee: []
created_date: '2026-07-08 20:24'
updated_date: '2026-07-08 20:40'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:refactor'
  - seq-2
dependencies: []
parent_task_id: TASK-170
ordinal: 192000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Replace VLoad256/VLoad512, VMov256/512, VLogic/VLogic256, VPackedBin/256 etc. with width-carrying ops (bytes or lane-count field). Halves the near-duplicate 256/512 arms in interp.rs + codegen.rs. Opportunistic — do families as they're touched by the masking work.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
