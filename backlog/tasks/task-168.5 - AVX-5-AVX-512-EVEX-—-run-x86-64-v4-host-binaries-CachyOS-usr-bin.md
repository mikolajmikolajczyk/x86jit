---
id: TASK-168.5
title: 'AVX-5: AVX-512/EVEX — run x86-64-v4 host binaries (CachyOS /usr/bin)'
status: In Progress
assignee: []
created_date: '2026-07-08 17:53'
updated_date: '2026-07-09 20:35'
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
OVERNIGHT PROGRESS 2026-07-09. Landed + pushed to main tonight (all green, clippy+fmt clean each): task-193 (ZMM/opmask captured in CpuSnapshot+compare+NativeOracle XSAVE — makes 512-bit/opmask paths comparable), task-168.5.1 (EVEX masked compares vpcmpeq/gt->k), task-168.5.2 (EVEX logic vpxorq/vpandq/vpord/vpandnq + vpternlog), task-168.5.4 (SSE4 gaps: pmovzx/sx, pmulld, blendv, round, dword min/max, AND pcmpistri/pcmpestri — the latter native-fuzzed against real silicon across all 96 imm8 modes), task-168.5.6 (lane ops vinserti32x4/64x2/64x4 + valignd/q), task-168.5.5 increment (masked EVEX logic). AC#1 (EVEX decode+ZMM+opmask state) and AC#3 (opmask ops + mask compares) substantially done; AC#2 partially (logic/compare masked; packed-arith masked + masked memory moves remain in 168.5.5). REMAINING toward AC#4/#5: finish 168.5.5 (masked packed arith + masked memory moves w/ fault suppression), task-195 (memory src2 + minor SSE4 ops), then AC#5 = advertise AVX-512F/BW/DQ/VL/CD in CPUID + run the real v4 corpus 3-way (the big integration step). NativeOracle now decodes EVEX faithfully so the whole AVX-512 surface is hardware-validatable.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
