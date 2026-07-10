---
id: TASK-168.5
title: 'AVX-5: AVX-512/EVEX — run x86-64-v4 host binaries (CachyOS /usr/bin)'
status: In Progress
assignee: []
created_date: '2026-07-08 17:53'
updated_date: '2026-07-10 08:02'
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
AC#5 STARTED 2026-07-10. Guest harness (guest.rs) gained .features(GuestCpuFeatures) → threads set_guest_cpu_features. First real AVX-512 integration: glibc_hello_avx512_interp_jit_agree runs hello_glibc under GuestCpuFeatures::v4() (advertises AVX-512F/BW/DQ/VL/CD) 3-way — glibc's IFUNC resolver selects its EVEX string routines, and interp==jit==native-reference (real AVX-512 CPU). Trap-and-fix closed one gap: vptestm/vptestnm{b,w,d,q} (glibc strlen/memchr zero-byte probe) — new IrOp::VPTestToMask (interp vptest_mask + jit emit_vptest_to_mask mirroring emit_vpcmp_to_mask, band+icmp-vs-zero → vhigh_bits → k). Tests: avx512_vptest_to_mask_match_interp (jit==interp v4), native_vptestnmb_matches_interp (real CPU). Suite 396/396, clippy+fmt clean. REMAINING for AC#5: broader corpus under v4 (only hello_glibc is glibc-dynamic; busybox/sqlite/djpeg are musl-static → no AVX-512 IFUNC; real CachyOS /usr/bin needs the OCI dynamic path). More glibc-heavy binaries would surface more EVEX gaps. Decision doc amending decision-11 still to write.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
