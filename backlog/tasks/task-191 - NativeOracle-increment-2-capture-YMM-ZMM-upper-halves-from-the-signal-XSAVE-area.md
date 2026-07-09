---
id: TASK-191
title: >-
  NativeOracle increment 2: capture YMM/ZMM upper halves from the signal XSAVE
  area
status: To Do
assignee: []
created_date: '2026-07-09 14:14'
labels:
  - code-review
  - 'crate:tests'
  - 'goal:test'
dependencies: []
ordinal: 215000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
run_native (native.rs, task-186) currently captures GPR/RIP/RFLAGS/XMM from the signal ucontext — enough for scalar/BMI/SSE2, and it already matches the fuzzer's current menu (legacy SSE2 leaves YMM upper untouched, so ymm_hi=0 both sides). Once the fuzzer emits AVX/AVX-512 (VEX/EVEX 256/512-bit ops — exactly what Unicorn can't oracle), the native oracle must also capture YMM upper 128 (and ZMM) from the XSAVE extended area in the signal frame: parse fpregs->sw_reserved for the xstate_bv + offsets, read the YMM_Hi128/ZMM components. Wire ymm_hi into the Capture struct + RunOutcome. Gate on host XSAVE/AVX support (skip => None if absent). This is the piece that makes the native oracle a true AVX-512 net for task-168.5.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 run_native captures YMM upper halves (and ZMM when present) from the signal XSAVE area
- [ ] #2 an AVX fuzzer snippet that writes ymm upper is oracled native-vs-interp
- [ ] #3 cargo nextest (--features unicorn) green minus fuzz_robustness; clippy -D warnings; fmt --check clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
