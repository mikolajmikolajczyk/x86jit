---
id: TASK-202
title: 'BUG: PyLong->double conversion wrong (float(int>=2^30)=0.0) under v4'
status: To Do
assignee: []
created_date: '2026-07-10 16:21'
updated_date: '2026-07-10 16:25'
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
- [ ] #1 isolate the faulting instruction (symbol'd _PyLong_Frexp reproducer or trace)
- [ ] #2 fix its lift; float(2**30)==1073741824.0 under --cpu v4
- [ ] #3 jit_eq_interp + native cross-check on the faulting op; suite green
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Further narrowing 2026-07-10: float(2**30+7)=14 (=7*2), float(2**30+42)=84 (=42*2), float(2**30+100)=72 (=(100*2)&0x7F=200-128). SIGNAL: mantissa extraction keeps only a low portion, shifted x2, high bits/digit truncated -> a variable-shift-count or bit_length-driven off-by-one in _PyLong_Frexp's digit-combine. RULED OUT via AVX-512 C reproducers (all match native): vcvtusi2sd/vcvtsi2sd, manual digit accumulate dx*2^30+digit, C <<'/>> variable shifts, BMI2 shlx/shrx/sarx, shld-style combine, bsr/lzcnt/clz. So the faulting instruction is in CPython 3.14's exact stripped sequence, not reproduced in isolation. NEXT: build python WITH symbols or an instruction-trace/single-step diff (interp step log) on float(2**30) to find the diverging op; OR extract _PyLong_Frexp source from CPython 3.14 and compile standalone -mavx512 to reproduce. This is the highest-value correctness bug remaining for numeric Python.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
