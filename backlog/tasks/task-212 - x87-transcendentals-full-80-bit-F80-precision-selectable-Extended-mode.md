---
id: TASK-212
title: 'x87 transcendentals: full-80-bit F80 precision (selectable, Extended mode)'
status: Done
assignee: []
created_date: '2026-07-11 11:21'
updated_date: '2026-07-11 11:37'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 241000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Follow-on to task-206 (which shipped the f64/libm 'Fast' path behind isolated F80::{sin,cos,tan,exp2m1,atan2,ylog2x,ylog2xp1} methods). Add the high-precision 'Extended' path: full-80-bit F80 transcendentals via range reduction + Taylor series (correct-by-construction, no external high-precision oracle available since native can't capture x87 and QEMU x87 isn't Intel-exact). Make precision selectable per-run (X87Precision {Fast, Extended}) threaded Vm->CpuState->exec_x87, the config seam the maintainer asked for (speed vs accuracy). Validate via F80 self-consistency identities (sin^2+cos^2=1, exp2(log2 x)=x to ~80-bit) which the f64 path cannot pass + f64-agreement + existing bounded-ULP differential stays green.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Extended F80 sin/cos/tan/exp2m1/atan2/log2 via reduction+Taylor, ~80-bit accurate
- [x] #2 X87Precision enum + Vm setter + CpuState field + exec_x87 dispatch; default Fast (zero behavior change)
- [x] #3 F80 identity tests (sin^2+cos^2, exp2/log2 inverse) hold to ~2 ULP-80 under Extended but NOT under Fast
- [x] #4 existing x87 differential + suite green; clippy+fmt clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11. Full-80-bit Extended x87 transcendentals. F80::{sin,cos,tan}_ext (quadrant reduction + Taylor), exp2m1_ext (expm1 Taylor, no cancellation), ylog2x_ext/ylog2xp1_ext (log2 via 2*atanh, fyl2xp1 avoids 1+x cancellation), atan2_ext (π/8-fold atan series + full quadrant). All F80 arithmetic (add/mul/div), correct-by-construction (factorial/odd-reciprocal terms, no minimax). Constants = x87 fldpi/fldln2/fldl2e 80-bit significands. Own round_to_i64 (to_i64_rc has a latent shift-overflow bug for |q| in [0.5,1) that reduce_quadrant would trip — noted, not fixed here). Config: X87Precision{Fast,Extended} in state.rs + CpuState field + Vm::set_x87_precision + new_vcpu seed + exec_x87 dispatch (default Fast = zero behavior change, existing x87 differential untouched). Validation (no 80-bit oracle exists): F80 identity tests — extended_beats_f64_on_identities proves sin^2+cos^2=1 to ~80-bit (residual exp<=-60) AND strictly tighter than Fast AND exp2(log2 x)=x ~80-bit; extended_transcendentals_round_to_libm (all fns within 1-2 ULP of libm); x87_extended_precision_selectable (end-to-end: Extended ST0 raw bytes DIFFER from Fast, both correct f64 — proves dispatch wired). Suite 549/549 (--features unicorn), clippy+fmt clean, aarch64 clean. No CLI flag (library API only; Extended differs only in low mantissa, not CLI-observable).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
