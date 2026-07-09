---
id: TASK-191
title: >-
  NativeOracle increment 2: capture YMM/ZMM upper halves from the signal XSAVE
  area
status: In Progress
assignee: []
created_date: '2026-07-09 14:14'
updated_date: '2026-07-09 17:15'
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
- [x] #1 run_native captures YMM upper halves (and ZMM when present) from the signal XSAVE area
- [x] #2 an AVX fuzzer snippet that writes ymm upper is oracled native-vs-interp
- [x] #3 cargo nextest (--features unicorn) green minus fuzz_robustness; clippy -D warnings; fmt --check clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
YMM upper-half capture landed (not yet committed). native.rs: handler reads the YMM_Hi128 component from the signal XSAVE area (magic1 + xstate_bv bit-2 gate; component offset from CPUID 0xD.2, passed to the child via the control page). Stub vzeroall's on an AVX host so an untouched YMM upper reads back 0 = the interpreter's zero-init (not the child's inherited-dirty FPU), which is why the existing 299-seed SSE2 native leg stayed green. Guard: nonzero ymm_hi init → None (the stub can't load a nonzero upper). RunOutcome.ymm_hi now carries the capture. Test native_captures_ymm_upper_half: vmovdqu ymm2,[pattern] → captured low+upper exact AND native==interp. Full suite 370/370 green (--features unicorn), clippy+fmt clean. ZMM (bits 511:256) + opmask k0-k7 deliberately NOT captured: the harness CpuSnapshot has no fields for them, so nothing to compare — split to task-193 (CpuSnapshot extension), which lands with the AVX-512 lift work (168.5.x).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
