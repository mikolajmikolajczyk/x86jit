---
id: TASK-195
title: >-
  AVX-512/SSE4 leftovers: memory src2 + minor ops (pmuldq, insertps, dpps,
  pblendw, pcmpistrm)
status: In Progress
assignee: []
created_date: '2026-07-09 20:34'
updated_date: '2026-07-10 13:47'
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
SESSION 5 2026-07-10 (post-merge, batch 3): merged origin (MODE-A compat32 + Exit::PortIo) verified healthy (nextest 474/474 pre-batch), then continued trap-and-fix. Broad+heavy real-work sweep of v4 /usr/bin (coreutils, grep/gawk/sed, less/vim, openssl/curl/git/gpg/ssh, ffmpeg/gs/clang/gcc/rustc, gzip/zstd/xz/bzip2/lz4/brotli/7z, sha*/b2sum/md5/cksum on 200KB blob, sort/od/base64) -> only ONE new instruction trap: cal hits EVEX-512 vpshufb zmm. Lifted VPshufbWide (helper->exec_vpshufb_wide, any width 128/256/512 + byte-granularity masking, register idx; mem-idx-512 deferred) — cal now prints calendar correctly, exit 0. Compat unchanged (Vpshufb mnemonic already covered via SSE/VEX forms). Tests: avx512_vpshufb_wide_match_interp (jit, unmasked+merge+zero) + native_vpshufb_wide_matches_interp. Corpus is now instruction-clean across the entire sampled v4 set under --cpu v4; remaining gaps = syscall shim only (task-93). STILL-DEFERRED lift gaps (none observed trapping): masked mem-dest narrowing, masked mem-src wide widening/vpabs/vpshufb-512, dword vpmin/max VEX/EVEX.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
