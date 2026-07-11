---
id: TASK-206
title: >-
  Lift x87 transcendentals: fsin/fcos/fptan/fpatan/f2xm1/fyl2x
  (UnknownInstruction)
status: In Progress
assignee: []
created_date: '2026-07-10 22:14'
updated_date: '2026-07-11 08:43'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 235000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Discovered while deepening the x87 differential (task-188): the interpreter does NOT lift the x87 transcendentals — fsin/fcos/fptan/fpatan/f2xm1/fyl2x/fyl2xp1/fsincos all trap UnknownInstruction. Present in real binaries via libm's x87 fallback paths. Needs F80 (80-bit) implementations (f80.rs) matching x87 semantics + reduction; validate against a documented reference (Unicorn's QEMU x87 transcendentals are not Intel-exact, so use a bounded-ULP guard vs libm, per the task-188 harness). Lower priority than SIMD gaps but a real ISA hole. task-188 left a tripwire test that fails when these get implemented, prompting a real differential.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
