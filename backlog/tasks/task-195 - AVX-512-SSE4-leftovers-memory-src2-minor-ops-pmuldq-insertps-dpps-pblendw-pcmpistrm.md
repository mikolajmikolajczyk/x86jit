---
id: TASK-195
title: >-
  AVX-512/SSE4 leftovers: memory src2 + minor ops (pmuldq, insertps, dpps,
  pblendw, pcmpistrm)
status: In Progress
assignee: []
created_date: '2026-07-09 20:34'
updated_date: '2026-07-10 11:46'
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
SESSION 3 2026-07-10: batch of 9 lifts closing real v4 /usr/bin instruction gaps (gate: nextest 417/417, clippy, fmt, compat regen). Added: opmask logic k{or,and,andn,xor,xnor}{b,w,d,q}+knot (VKBinOp/VKNot); VEX vpunpck{l,h}* (lift_vunpack_avx); VEX vcvt{ss2sd,sd2ss} (lift_vcvt_scalar); VEX vpsrldq/vpslldq (lift_byteshift_avx); EVEX narrowing vpmov{qd,qw,qb,dw,db,wb} (VPmovNarrow helper); masked packed arith vp{add,sub}{d,q}{k}{z} (VMaskedPacked helper, task-168.5.5); EVEX vrndscale{ss,sd} M=0 (lift_vrndscale); AC#3-adjacent MEMORY-SOURCE pcmpistri/pcmpestri (VPcmpStrM helper — the deferred-hard one; loads u128 in JIT via checked_addr+gload, pcmpstr_run_bv shared). Running 3-way --cpu v4 verified-correct: tac(c b a)/wc(2 4 20)/head/cut/gawk(42,int7.9=7)/grep/find/sort/sed. NATIVE CROSS-CHECK caught a real bug: vpminud (dword unsigned min VEX/EVEX) is NOT dispatched (only Vpminub/Vpminuq) so a jit-only test falsely passed (UnknownInstruction both sides = agree); native-vs-real-CPU exposed it. Tests use dispatched vpaddq instead. STILL-DEFERRED: EVEX-512 widening vpmovsxdq zmm<-ymm (less, next); pcmpistrm/pcmpestrm mask forms (AC#3); vpmin/max dword VEX/EVEX (Vpminud/Vpmaxud/Vpminsd/Vpmaxsd — new small gap); masked mem-src logic/ternlog. Corpus blockers now dominated by syscall shim (task-93): 221 fadvise64, 27 mincore, 145, 99 — embedder scope, non-fatal (binaries still produce correct output).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
