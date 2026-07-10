---
id: TASK-210
title: >-
  Lift GFNI + SSSE3 psign: gf2p8affineqb/gf2p8affineinvqb/gf2p8mulb, psignb/w/d
  (+VEX)
status: Done
assignee: []
created_date: '2026-07-10 22:55'
updated_date: '2026-07-10 23:15'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 239000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real v4 binaries: psignw 112, vpsignd 20, psignd 18 (SSSE3 sign); gf2p8affineqb present (GFNI, host-supported). psign: per-element, negate src if ctrl<0, zero if ctrl==0, keep if ctrl>0. GFNI: gf2p8mulb = GF(2^8) mul mod 0x11B per byte; gf2p8affineqb = affine transform (8x8 bit-matrix from imm/operand) then XOR. Native bit-exact validate (host gfni). Found via 2026-07-11 trap-and-fix recon.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 SSSE3 psignb/psignw/psignd (+ VEX vpsign*) lifted: dst[i] = sign(ctrl[i]) applied to src[i] (negate/zero/keep)
- [x] #2 GFNI gf2p8mulb (GF(2^8) byte multiply), gf2p8affineqb + gf2p8affineinvqb (affine transform, imm8 constant) lifted
- [x] #3 interp==jit; native bit-exact (host has gfni); differential per op
- [x] #4 suite green; clippy+fmt
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
