---
id: TASK-187
title: >-
  Coverage ratchet: assert every lifted op has a differential test or
  fuzzer-menu entry
status: To Do
assignee: []
created_date: '2026-07-09 12:51'
labels:
  - code-review
dependencies: []
ordinal: 211000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The compat map (x86jit-tests/src/compat.rs) tracks PRESENCE (does the op lift) not CORRECTNESS, and forces a coverage.json regen when a lift arm is added — but nothing forces the new op to have a CORRECTNESS test. Add a ratchet: a test/list that maps each lifted IrOp family (or iced Code class) to at least one differential test or a fuzzer-generator entry, and fails when a newly-lifted op has neither. Prevents new instructions from shipping with zero correctness coverage. Pairs with the fuzzer extension (task-185) and NativeOracle.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
