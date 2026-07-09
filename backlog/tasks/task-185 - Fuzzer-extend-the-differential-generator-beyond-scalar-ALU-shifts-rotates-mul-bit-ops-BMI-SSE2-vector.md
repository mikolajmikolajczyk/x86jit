---
id: TASK-185
title: >-
  Fuzzer: extend the differential generator beyond scalar ALU
  (shifts/rotates/mul/bit-ops/BMI + SSE2 vector)
status: In Progress
assignee: []
created_date: '2026-07-09 12:51'
updated_date: '2026-07-09 13:14'
labels:
  - code-review
dependencies: []
ordinal: 209000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The differential fuzzer (x86jit-tests/src/fuzz.rs) generates only scalar ALU/mov (FuzzInsn: add/sub/adc/sbb/and/or/xor/cmp/test, inc/dec/neg/not, mov/movzx/movsx, setcc/cmov, load/store). Everything else — shifts/rotates/shld-shrd, mul/imul, bt*/bsf/bsr/popcnt/bswap/tzcnt/lzcnt, BMI, and every vector/SSE/AVX op — has NO randomized coverage; a new instruction gets zero automatic jit==interp OR Unicorn-oracle coverage until someone hand-writes a snippet. The compare() already checks full state (gpr+flags+xmm+ymm+mem) and shrink() is generic, so new FuzzInsn variants are compared + auto-shrunk for free, and the unicorn leg is a REAL-CPU oracle for everything Unicorn decodes (legacy SSE, shifts, mul, bit-ops, BMI — NOT VEX). Extend the generator: (1) shifts/rotates by imm and CL + shld/shrd (subtle OF/CF/count-0 flags — silent-regression risk #4); (2) mul/imul (1/2/3-op), bt/bts/btr/btc, bsf/bsr, popcnt, bswap, tzcnt/lzcnt; (3) BMI1/2 (andn/blsi/blsr/blsmsk/bextr/bzhi/pdep/pext/shlx/shrx/sarx/rorx/mulx); (4) SSE2 packed-integer with XMM seeding (padd/psub/pand/por/pxor/pandn/pcmpeq/pcmpgt/punpck/pshufd/psll/psrl/psra-imm/movdqa/pmovmskb/pack). Exclude div/idiv (faults) and packed-float NaN (needs masked compare) in the first cut. Seed xmm registers in gen(). Verify: cargo nextest run binary(fuzz) + --features unicorn; any divergence surfaced is a real bug to fix. Do NOT extend to VEX/EVEX — those need the NativeOracle (separate task) since Unicorn drops VEX.vvvv.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Increment 1 DONE (scalar-extended): fuzzer generates shl/shr/sar/rol/ror/rcl/rcr (imm+CL), shld/shrd, mul/imul (1/2/3-op), bt/bts/btr/btc, tzcnt/lzcnt, popcnt, bswap, BMI1 (andn/blsi/blsr/blsmsk), shlx/shrx/sarx, rorx, mulx. Added: (a) per-program undefined-flag mask (flag_effect table); (b) flag-consumer gating in gen() so an undefined flag never reaches cmov/setcc/adc/sbb/rcl/rcr. jit==interp (600 seeds) + unicorn real-CPU oracle (300) both green. FINDINGS: (1) real lifter gap mul r8/imul r8 (8-bit) unlifted -> filed. (2) Interp validated CORRECT vs TWO QEMU bugs confirmed on real hw: bzhi index-clamp + pdep/pext missing 32-bit zero-extend; so Unicorn cant oracle BMI2 (dropped; needs NativeOracle task-186). TODO increment 2: SSE2 vector ops + XMM seeding.
<!-- SECTION:NOTES:END -->
