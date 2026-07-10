---
id: TASK-202
title: 'BUG: PyLong->double conversion wrong (float(int>=2^30)=0.0) under v4'
status: Done
assignee: []
created_date: '2026-07-10 16:21'
updated_date: '2026-07-10 21:10'
labels:
  - 'crate:core'
  - 'goal:bug'
dependencies: []
ordinal: 231000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real v4 python3.14 (--cpu v4): converting any Python int >= 2^30 to double yields 0.0. MINIMAL REPRO: float(2**30) -> 0.0 (host 1073741824.0); float(2**29) -> correct (fits one 30-bit CPython digit). Also (2**30)*1.0=0.0, 2**30>1.5=False (int coerced to 0.0). int(float) is FINE (float->int works). SHARED interp+jit (both wrong) => a lifted-instruction SEMANTIC bug, not codegen. ISOLATED C int->double converts verified CORRECT vs native incl. vcvtusi2sd/vcvtsi2sd, manual digit accumulate (dx*2^30+digit), variable shifts, shld/shrd-style combine, clz -> all match. So the fault is a specific instruction in CPython 3.14's multi-digit PyLong_AsDouble/_PyLong_Frexp path not yet reproduced in isolation (binary is stripped -> needs instruction-level trace or a symbol'd _PyLong_Frexp reproducer compiled with -mavx512). IMPACT: breaks statistics.stdev (gave ~9e-7 instead of 30.7), any big-int->float division, scientific/numeric Python. Discovered while validating FMA (task-201): FMA itself is bit-exact; this is orthogonal. NOT MXCSR (math.sqrt/exp correct).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 isolate the faulting instruction (symbol'd _PyLong_Frexp reproducer or trace)
- [x] #2 fix its lift; float(2**30)==1073741824.0 under --cpu v4
- [x] #3 jit_eq_interp + native cross-check on the faulting op; suite green
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
ROOT CAUSE FOUND + FIXED 2026-07-10. Self-locating interp trace of CPython _PyLong_Frexp (prologue byte-signature detection, --backend interp) pinpointed the diverging op: 'vaddsd xmm0,xmm1,xmm0' in the Horner digit-accumulate. lift_vfloat_bin did an UNCONDITIONAL 'VMov dst<-op1' then VFloatBin{a:dst,b:op2}; when op2 aliases dst (op2==dst), the VMov clobbered op2 before it was read, yielding op1+op1 (iter A: 2*x_digits[1]) then op1+0 dropping the big term (iter B), so dx collapsed to 0 -> float=0.0. FIX: register-op2 branch now passes op1/op2 straight to the non-destructive 3-operand VFloatBin (no pre-copy); VMov kept only in the memory branch (memory can't alias a reg). This matches the already-correct pattern in lift_vlogic_vex/lift_vpacked_bin_vex. VERIFIED bit-exact host py3.13 == jit v4 == interp v4 on float(2**30), +7/+100/+128, 2**53/2**60, 3*2**30, 2**64-1; statistics.stdev correct. Tests: jit.rs vex_float_bin_dst_aliases_src2_match_interp (jit==interp, reg+mem, add/sub/mul/div/min/max/packed) + native.rs native_vaddsd_dst_aliases_src2_matches_interp (real CPU). NOT vcvtusi2sd (probed, correct). SIBLING latent bugs (same mechanism, other ops) filed as task-203.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
