---
id: TASK-201
title: 'AVX-512/FMA: vfmadd/vfmsub/vfnmadd/vfnmsub {132,213,231}{ss,sd,ps,pd}'
status: In Progress
assignee: []
created_date: '2026-07-10 14:48'
updated_date: '2026-07-10 21:10'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
ordinal: 230000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
FMA3 fused multiply-add subsystem (~48 encodings). Blocks heavy python3 (statistics/numpy-like), scientific/ML workloads under --cpu v4. Natural follow-on to task-195 (all non-FMA sampled v4 /usr/bin now instruction-clean). Needs: FloatBinOp-style 3-input op (a*b+c with the 132/213/231 operand-order variants + negate-product/negate-addend), scalar (ss/sd) + packed (ps/pd) any width, register+memory src, masked EVEX forms; jit_eq_interp(v4) + native cross-check per representative; compat regen.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vfmadd/sub scalar+packed lifted (132/213/231 orders)
- [x] #2 vfnmadd/vfnmsub variants lifted
- [ ] #3 memory src + masked EVEX forms
- [x] #4 differential + native cross-check; compat regen; suite green; clippy+fmt
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
FMA3 core implemented + validated 2026-07-10. VFma/VFmaM ops via shared fma_lanes (f64/f32 mul_add = single fused rounding); exec_fma (reg helper) + fma_mem_run<StrMem> (fault-capable mem helper). Lift resolves 132/213/231 -> x/y/z roles (op0=dst,op1=vvvv,op2=reg/mem; mem always op2 -> y for 132/231, z for 213). All 48 mnemonics dispatched (vf[n]m{add,sub}{132,213,231}{ss,sd,ps,pd}). VALIDATED: native_fma_matches_interp covers all 4 types + 3 orders + 4 signs + memory operands vs REAL CPU -> pass; fma_all_variants_match_interp (jit==interp). Basic/moderate float arithmetic now correct under --cpu v4 (e.g. sum(1/i)=7.484471 matches host). ALSO added Vstmxcsr/Vldmxcsr (VEX aliases of the existing stmxcsr-writes-default / ldmxcsr-noop). REMAINING (AC#3 masked EVEX FMA): deferred (evex_is_masked -> unsupported). DOWNSTREAM NON-FMA ISSUE: python3 math.* / statistics give wrong/empty results because MXCSR rounding-mode is NOT modeled (ldmxcsr is a no-op) -> libm's temporary rounding-mode changes ignored. That is task-101 (MXCSR modeling), NOT an FMA bug. FMA itself is bit-exact vs hardware.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
