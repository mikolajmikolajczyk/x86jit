---
id: TASK-185
title: >-
  Fuzzer: extend the differential generator beyond scalar ALU
  (shifts/rotates/mul/bit-ops/BMI + SSE2 vector)
status: In Progress
assignee: []
created_date: '2026-07-09 12:51'
updated_date: '2026-07-09 13:29'
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
Increment 2 DONE (SSE2 vector): fuzzer now seeds xmm0..7 and generates SSE2 packed-integer ops — padd/psub b/w/d/q, pand/por/pxor/pandn, pcmpeq/pcmpgt b/w/d, punpckl/h all 8, packuswb, pminub/pmaxub, psll/psrl/psra{w,d,q} by imm, pshufd, pmovmskb. jit==interp (600 seeds) + Unicorn real-CPU oracle (300) green. Restricted VBin to the 29 LIFTED ops; the unlifted SSE2 packed ops (packsswb/packssdw, pmuludq/pmaddwd/pmulhw/pmullw, pavgb/w, paddusb/usw/psubusb/usw, paddsb/sw/psubsb/sw) filed as a gap. Also found + verified on real hardware a 3rd QEMU bug: shld/shrd with count 0 wrongly clears flags in QEMU (interp correct) -> DoubleShift constrained to nonzero immediate counts. NET: fuzzer went scalar-ALU-only -> broad scalar+SSE2 ISA differential with per-program undefined-flag masking and flag-consumer gating; validated interp correct against 3 QEMU bugs (bzhi, pdep/pext, shld-count-0) + found 1 lifter gap (mul r8) + 12 unlifted SSE2 ops. Remaining: AVX/AVX2/AVX-512 need the NativeOracle (task-186) since Unicorn drops VEX.vvvv.
<!-- SECTION:NOTES:END -->
