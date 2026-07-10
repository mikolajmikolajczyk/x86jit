---
id: TASK-195
title: >-
  AVX-512/SSE4 leftovers: memory src2 + minor ops (pmuldq, insertps, dpps,
  pblendw, pcmpistrm)
status: In Progress
assignee: []
created_date: '2026-07-09 20:34'
updated_date: '2026-07-10 12:26'
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
SESSION 4 2026-07-10 (batch 2): 5 more lifts closing less/openssl/vim/curl (gate: nextest 423/423, clippy, fmt, compat regen v4 covered +22). Added: EVEX/VEX-256 widening vpmov{s,z}x* to ymm/zmm or masked xmm dest (VPMovExtendWide helper — less: vpmovsxdq zmm<-ymm); AVX-512DQ vpmullq 64-bit multiply-low (new PackedBinOp::MulLo64 wired into packed_bin/emit_packed_bin/exec_masked_packed — openssl/curl); packed abs vpabs{b,w,d,q} (VPAbs helper, any width+mask — vim); opmask shift kshift{l,r}{b,w,d,q} (VKShift, inline, imm>=width->0 guard — vim); EVEX narrowing store to MEMORY vpmov{q,d,w}{d,w,b} [mem],src unmasked (VPmovNarrowMem + fault-capable vpmov_narrow_mem_helper via shared narrow_store_run<StrMem> — curl: vpmovqd [mem],xmm). ALL sampled v4 /usr/bin now instruction-clean under --cpu v4: tac/wc/head/cut/gawk/grep/find/sort/sed/less/openssl/vim/curl/git/python3/perl/tar/zstd (verified --version/basic runs). Tests: 3 jit_eq_interp(v4) + 3 native_*_matches_interp. STILL-DEFERRED: masked memory-dest narrowing (per-lane fault suppression); masked mem-src for wide widening/vpabs; vpmin/max DWORD VEX/EVEX (Vpminud/Vpmaxud/Vpminsd/Vpmaxsd — still undispatched, noted). Remaining corpus blockers = syscall shim only (task-93, non-fatal).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
