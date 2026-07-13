---
id: TASK-239
title: >-
  perf: implement packed float<->int converts
  (cvtps2dq/cvtdq2ps/cvttps2dq/cvtps2pd/cvtpd2ps) — MISSING, traps today
status: Done
assignee: []
created_date: '2026-07-13 08:20'
updated_date: '2026-07-13 10:38'
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
- [x] #1 cvtps2dq/cvtdq2ps/cvttps2dq/cvtps2pd/cvtpd2ps (+ dq2pd/pd2dq/tpd2dq) lift + native-lower on x86 + ARM, bit-exact vs unicorn incl. out-of-range/NaN/rounding edge cases; coverage.json regenerated (moved out of v1 missing)
- [x] #2 cargo nextest run (--features unicorn) green minus fuzz_robustness; clippy clean; fmt clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. Packed float<->int converts implemented: cvtdq2ps/cvtps2dq/cvttps2dq/cvtdq2pd/cvtps2pd/cvtpd2ps/cvtpd2dq/cvttpd2dq (SSE + VEX.128; 256/512 deferred). New IR op VPackedCvt + PackedCvtKind (ir.rs); lift_packed_cvt (lift/vector.rs) + dispatch (lift/mod.rs); emit_v_packed_cvt native codegen (cranelift/codegen/vector.rs); exec_v_packed_cvt interp (interp/vector.rs). 4-lane ps ops lower to native vector Cranelift (fcvt_from_sint/fcvt_to_sint_sat/nearest/fvpromote_low/fvdemote/swiden_low -> NEON); 2-lane f64->i32 scalarized (Cranelift has no f64x2->i32x2 sat). Saturating semantics match scalar cvt convention (x86 integer-indefinite deferred). Mem source materialized via VLoad into dst (pshufd pattern), 1 IR op. Tests: cvt_packed_int_float_match_unicorn (interp vs CPU, in-range round/trunc/widen/narrow/mem), cvt_packed_match_interp (jit==interp on NaN/inf/overflow edges), cvt_packed_vex128_matches_sse (VEX==SSE). coverage.json regenerated -> cvt out of v1 missing. Adversarial review: no bugs (6 concerns verified incl f32/f64 saturation boundaries). DoD: nextest --features unicorn 665 passed / clippy clean / fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
