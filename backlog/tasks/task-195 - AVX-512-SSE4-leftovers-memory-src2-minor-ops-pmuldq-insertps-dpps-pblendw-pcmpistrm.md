---
id: TASK-195
title: >-
  AVX-512/SSE4 leftovers: memory src2 + minor ops (pmuldq, insertps, dpps,
  pblendw, pcmpistrm)
status: In Progress
assignee: []
created_date: '2026-07-09 20:34'
updated_date: '2026-07-10 10:30'
labels:
  - code-review
  - 'crate:core'
  - 'goal:m8-simd'
dependencies: []
ordinal: 219000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Follow-ups deferred while landing task-168.5.1/2/4/6. Memory-source src2 for the newly lifted ops (EVEX vpcmpeq/gt, vpxorq/vpternlog, pmovzx/blendv/round/pcmpistri, vinsert/valign) — all currently register-src-only. Minor SSE4.1/4.2 not yet lifted: pmuldq/pmuludq (widening 32x32->64), insertps (imm8 dword insert + zmask), dpps/dppd (dot product), pblendw (imm8 word blend), and the pcmpistrm/pcmpestrm mask-producing string forms (pcmpistri/pcmpestri index forms are done). Lower frequency; not blocking the v4 corpus. Each needs a differential + native cross-check + compat regen.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 memory-source src2 lifted for the task-168.5.x ops that are register-only
- [ ] #2 pmuldq/pmuludq, insertps, pblendw lifted (dpps/dppd if a corpus binary needs them)
- [ ] #3 pcmpistrm/pcmpestrm mask forms lifted
- [ ] #4 differential + native cross-check per op; compat regenerated; suite green; clippy+fmt clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
SESSION 2 (2026-07-10 cont): trap-and-fix drove /usr/bin/sort to completion under --cpu v4 (sorts correctly). Now running 3-way: true, echo, base64, sort, tr, nl, uniq. seq/sha256 blocked on x87 (dd fld) — separate subsystem, not AVX-512.

OPS ADDED THIS SESSION (interp+jit+lift, HW-validated where native-testable):
  - vpopcnt{d,q} (VPopcnt/VPopcntM, inline per-lane popcnt).
  - kunpck{bw,wd,dq} opmask interleave (VKUnpack).
  - vpermt2{b,w,d,q} two-table permute + masked (VPermT2, helper->interp exec_vpermt2).
  - vextracti/f{32x4,64x2,32x8,64x4} lane extract (VExtractLaneWide).
  - VEX-128 scalar/packed float arith v{add,sub,mul,div,min,max}{ss,sd,ps,pd} (lift_vfloat_bin: VMov merge + SSE VFloatBin + VZeroUpper).
  - VEX vmovs{s,d} (store / 2-op load+zero / 3-op merge), vmov{l,h}p{s,d}, vcomis*/vucomis* (alias), vpshufd, vcvtsi2s* / vcvttsd2usi etc.
  - unsigned conversions: VCvtToInt + VCvtFromInt gained a signed:bool field; cvt(t)s*2usi + cvtusi2s* (AVX-512).

TESTS: jit avx512_permute_popcnt_kunpck / avx512_vex_float_and_unsigned_cvt; native native_vpopcnt_vpermt2 (real CPU, needs avx512vpopcntdq). compat regenerated (228 lines off missing). NOTE: v4() stays STRICT (does NOT advertise VPOPCNTDQ); vpopcntq executes anyway because CachyOS builds beyond strict-v4 and emit it unconditionally.

GATES: clippy -D clean, fmt clean; full nextest running. NOT yet committed.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
