---
id: TASK-195
title: >-
  AVX-512/SSE4 leftovers: memory src2 + minor ops (pmuldq, insertps, dpps,
  pblendw, pcmpistrm)
status: In Progress
assignee: []
created_date: '2026-07-09 20:34'
updated_date: '2026-07-10 14:54'
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
SESSION 6 2026-07-10 (batch 4, deep runtime sweep): drove language runtimes on real workloads (python3/perl/lua/sqlite/node). Closed 9 instruction gaps: dword packed min/max vpmin/max{u,s}d VEX+EVEX (perl/python3); vpermi2{b,w,d,q} index-mode permute (added imode flag to VPermT2/exec_vpermt2); vpermt2 MEMORY src2 (VPermT2M + fault-capable helper via shared permute2_run<StrMem>); vinserti128 MEMORY src (VInsert128M); vpshufd ymm/zmm wide+masked (VShuffle32Wide helper); vpblendw VEX.128 (VBlendW via byte-shuffle); saturating packs vpack{ss,us}{wb,dw} VEX/EVEX (VPackWide helper, source read signed); vsqrtsd/ss VEX scalar (lift_vfloat_unary_scalar); single-source vector-index vpermq/vpermd (VPerm1 helper). Basic + moderate python3 now runs (sha256+sqrt+statistics-ish). NATURAL BOUNDARY: heavy python3 needs FMA3 (vfmadd132sd...) -> new task-201 (whole ~48-encoding subsystem, deferred). Tests: 4 jit_eq_interp(v4) + 3 native_*_matches_interp covering the batch. Gate green. NOTE: native cross-check remains essential (caught the vpminud false-pass in an earlier session).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
