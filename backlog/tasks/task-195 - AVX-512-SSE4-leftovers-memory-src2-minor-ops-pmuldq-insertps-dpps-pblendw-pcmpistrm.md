---
id: TASK-195
title: >-
  AVX-512/SSE4 leftovers: memory src2 + minor ops (pmuldq, insertps, dpps,
  pblendw, pcmpistrm)
status: In Progress
assignee: []
created_date: '2026-07-09 20:34'
updated_date: '2026-07-10 09:58'
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
GATES GREEN 2026-07-10 (not yet committed, awaiting user OK):
  - cargo nextest --features unicorn: 405/405 passed, 2 skipped (incl fuzz_robustness + go_http + 6 new tests).
  - cargo clippy --all-targets --all-features -D warnings: clean.
  - cargo fmt --all --check: clean.
  - compat regenerated (idempotent); vpshufd/vpcmpistri/vpcmpestri off missing.

3-way under --cpu v4 (AVX-512), interp==jit==real-CPU where native-testable:
  /usr/bin/true exit0, /usr/bin/echo hi -> hi, /usr/bin/base64 x86jit -> eDg2aml0 (correct).

New tests: jit avx512_vpcmp_vptest_mem_src / avx512_masked_mem_move / avx512_mem_src_data_ops; native native_evex_vpcmp_mem_src / native_masked_mem_move / native_evex_512_mem_src.

NEXT (not blocking this unit): seq -> x87 (dd fld, separate subsystem); sort -> vpopcntq (new AVX512-VPOPCNTDQ op; also verify why glibc dispatched it under v4 CPUID). Still deferred: masked mem-src LOGIC/ternlog; 128/256 EVEX high-reg packed via wide op if a trap needs it.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
