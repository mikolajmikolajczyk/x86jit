---
id: TASK-168.5
title: 'AVX-5: AVX-512/EVEX — run x86-64-v4 host binaries (CachyOS /usr/bin)'
status: In Progress
assignee: []
created_date: '2026-07-08 17:53'
updated_date: '2026-07-08 18:06'
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
IN PROGRESS. Foundation landed: state widened to 32 vector regs (EVEX addresses XMM/YMM/ZMM 16-31), added zmm_hi ([[u128;2];32], bits 511:256) + kmask (k0-k7) to CpuState/jit_abi/Vcpu; VZeroUpper now clears bits 511:128 (AVX-512 semantics). EVEX decode via iced (op_mask/zeroing/reg_zmm/evex_is_masked helper). First ops: unmasked 512-bit vmovdqu/vmovdqa (VLoad512/VStore512/VMov512) interp+cranelift, jit==interp test avx512_vmovdqu512_load_mov_store_match_interp. /usr/bin/true (CachyOS v4) now clears its 512-bit ZMM stores. NEXT (trap-and-fix on host v4 binaries): vpinsrq/vpinsrd + VEX vpinsr/vpextr (SSE4.1-class VEX gaps, not AVX-512 but block v4 binaries), then masked 512 moves (k1-k7/zeroing), vpcmp->k, kmov/kortest, vpternlog, vpbroadcast/broadcasts EVEX, 128/256 EVEX forms. Masked ops deferred (evex_is_masked returns unsupported). CPUID does NOT yet advertise AVX-512 (gate last, mirrors 168.4). Corpus/jit/compat all green.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
