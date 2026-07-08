---
id: TASK-168.5
title: 'AVX-5: AVX-512/EVEX — run x86-64-v4 host binaries (CachyOS /usr/bin)'
status: In Progress
assignee: []
created_date: '2026-07-08 17:53'
updated_date: '2026-07-08 18:27'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168
ordinal: 182000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Extend the SIMD lifter from VEX/AVX2 (task-168, done) to EVEX/AVX-512 so x86-64-v4 host binaries run — CachyOS /usr/bin are v4-optimized (AVX-512F/BW/DQ/VL/CD). EVEX is a strictly larger surface than VEX: a 4-byte 62h prefix, 32 vector regs (ZMM0-31 at 512-bit), 8 opmask registers (k0-k7) for per-element predication/zeroing, embedded broadcast, and embedded rounding/SAE. Big state + IR + backend widening, comparable in size to all of 168. Gate advertisement LAST (mirrors 168.4) — advertising AVX-512 before lifting is solid turns the whole glibc/distro corpus onto EVEX paths that would #UD on any unlifted op.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 EVEX 62h decode + ZMM0-31 (512-bit) + k0-k7 opmask state land in CpuState/jit_abi/test harness
- [ ] #2 Masked/zeroing 512-bit data-mov + logic + packed integer arith lifted (interp==jit); 128/256 EVEX forms reuse existing YMM paths where possible
- [ ] #3 Opmask ops (kmov/kand/kor/kortest/ktest/knot) + mask-producing compares (vpcmpb/w/d/q -> k) lifted
- [ ] #4 AVX-512 specials the v4 glibc/distro corpus actually uses covered (vpternlog, vpcmp, broadcasts, cross-lane permutes, vpblendm); driven by real-binary trap-and-fix loop
- [ ] #5 CPUID advertises AVX-512F/BW/DQ/VL/CD; the full real-binary corpus stays green 3-way with glibc/distro on AVX-512 paths; a decision doc amends decision-11
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Opmask compare/test machinery landed. Added: vpcmp{,u}{b,w,d,q} -> opmask k (predicates EQ/LT/LE/FALSE/NE/GE/GT/TRUE, signed+unsigned, 128/256/512, WITH EVEX writemask k1-k7), and kortest{b,w,d,q} (ZF=OR==0, CF=OR==allones). New IR VPCmpToMask{...,writemask}, VKOrTest; interp+cranelift (vhigh_bits per-lane bit extraction); jit==interp test avx512_vpcmp_kortest_match_interp. /usr/bin/true now clears ALL its EVEX instructions and reaches glibc's CPU-level check: 'Fatal glibc error: CPU does not support x86-64-v2' (exit 127). KEY FINDING: running v4 static binaries is GATED ON THE CPUID-ADVERTISE STEP (AC#5) — glibc reads the feature level from CPUID and refuses before executing, independent of instruction coverage. To run them: advertise x86-64-v2/v3/v4 baseline (SSE4.2/AVX/AVX512F/BW/VL/DQ/CD) — which collides with decision-2 (SSE4 dropped, pcmpistri unlifted) and needs the full corpus verify-loop. REMAINING before advertise: masked/zeroing EVEX data ops (vmovdqu32 k1 etc), kmov/kand/knot/kshift, vpternlog, SSE4.2 baseline (pcmpistri/pcmpestri). CPUID still NOT advertising AVX-512.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
