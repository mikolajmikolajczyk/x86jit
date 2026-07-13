---
id: TASK-239
title: >-
  perf: implement packed float<->int converts
  (cvtps2dq/cvtdq2ps/cvttps2dq/cvtps2pd/cvtpd2ps) — MISSING, traps today
status: To Do
assignee: []
created_date: '2026-07-13 08:20'
labels:
  - 'crate:cranelift'
  - 'crate:core'
  - 'goal:perf'
milestone: ps4-perf
dependencies: []
ordinal: 268000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
ps4-perf Tier-1, from the task-236 audit (backlog/docs/doc-34, worklist #1). Packed float<->int converts are UNIMPLEMENTED and trap as unknown instructions: cvtps2dq, cvtdq2ps, cvttps2dq, cvtps2pd, cvtpd2ps (+ cvtdq2pd, cvtpd2dq, cvttpd2dq). They are in the x86-64-v1 (baseline SSE2) 'missing' list of coverage.json -- present on every x86-64 CPU incl. PS4's Jaguar, and ubiquitous in game code (per-vertex int<->float, colour/normal packing, fixed-point). A guest hitting one traps today. This is both HOT and BROKEN -> highest-value SIMD item. Native-lowerable directly (no helper): vector fcvt_to_sint_sat / fcvt_from_sint / fpromote / fdemote -- the scalar cvt forms already use exactly these (x86jit-cranelift/src/codegen/vector.rs:2914-3016). Wire lift (x86jit-core/src/lift/) + IR op + native codegen. Mind MXCSR rounding (cvtps2dq uses current rounding, cvttps2dq truncates) and x86 out-of-range/NaN -> 0x8000_0000 indefinite-integer semantics (fcvt_to_sint_sat saturates differently -- match x86, validate vs unicorn). Add an int<->float packed microbench to task-235's suite.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 cvtps2dq/cvtdq2ps/cvttps2dq/cvtps2pd/cvtpd2ps (+ dq2pd/pd2dq/tpd2dq) lift + native-lower on x86 + ARM, bit-exact vs unicorn incl. out-of-range/NaN/rounding edge cases; coverage.json regenerated (moved out of v1 missing)
- [ ] #2 cargo nextest run (--features unicorn) green minus fuzz_robustness; clippy clean; fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
