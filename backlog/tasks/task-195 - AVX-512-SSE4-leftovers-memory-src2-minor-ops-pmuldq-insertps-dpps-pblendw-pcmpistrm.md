---
id: TASK-195
title: >-
  AVX-512/SSE4 leftovers: memory src2 + minor ops (pmuldq, insertps, dpps,
  pblendw, pcmpistrm)
status: In Progress
assignee: []
created_date: '2026-07-09 20:34'
updated_date: '2026-07-10 10:46'
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
SESSION 2 FINAL 2026-07-10: 3 commits pushed (73b1614 mem-src EVEX + masked-mem; 74589c6 vpopcnt/vpermt2/kunpck/VEX-float/unsigned-cvt; 1a2081e x87 fisttp + VEX pmovzx). All gates green each commit (405/407 nextest, clippy, fmt).

RUNNING 3-way under --cpu v4 (AVX-512): true, echo, base64, sort, tr, nl, uniq, seq, factor. Verified-correct output (base64/sort/seq/factor).

REMAINING corpus blockers (NOT instruction-lift gaps I should chase further here):
  1. tac -> memory-source pcmpistri (vpcmpistri xmm, [mem], imm). DEFERRED-HARD: the VPcmpStr helper reads cpu.xmm[b] by index; a memory operand needs either a scratch-vector mechanism or a VPcmpStrM variant passing the loaded u128 value through the helper ABI. Also pcmpistrm/pcmpestrm (mask-producing) still unlifted (task-195 AC#3).
  2. wc/sha256 -> syscall 221 (fadvise64) + others (145 sched_getscheduler, 99 sysinfo) -> ENOSYS. This is the EMBEDDER syscall shim (x86jit-linux/src/shim.rs), task-93 scope, NOT the lifter. fadvise64 could no-op to 0 but may not be the sole blocker; needs shim debugging.

Still-deferred instruction work in-scope for task-195: masked mem-src logic/ternlog; VEX-256 float arith (ymm); memory-src for vpermt2/vpopcnt; pcmpistrm/estrm. None block the 9 running binaries.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
