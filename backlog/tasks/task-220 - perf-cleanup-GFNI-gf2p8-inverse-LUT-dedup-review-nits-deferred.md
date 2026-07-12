---
id: TASK-220
title: 'perf/cleanup: GFNI gf2p8 inverse LUT + dedup review nits (deferred)'
status: To Do
assignee: []
created_date: '2026-07-12 07:17'
labels:
  - 'crate:core'
  - 'crate:linux'
  - cleanup
dependencies: []
ordinal: 249000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Lower-priority code-review nits on task-215, safe to defer. (1) crate::gfni gf2p8affineinvqb computes the GF(2^8) multiplicative inverse by an O(255) brute-force search per byte; now driven over full ZMM (64 bytes) in openssl's hot vectorized-AES loop. A one-time 256-byte inverse LUT (or folding it into the affine composition) turns each byte into an array index. (2) The 16-byte struct-iovec walk (read_u64(iov+i*16) / +8) is copy-pasted across SYS_READV, SYS_WRITEV, SYS_SENDMSG, SYS_RECVMSG — extract a read_iovecs helper. (3) var_shift_one re-derives packed_shift's over-shift/sign-extend edge-cases, and exec_gf2p8 vs gf2p8_mem_run share an identical lane loop + k==0/masked writeback — factor the shared body. All three are cleanup, not correctness.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 gf2p8 inverse no longer O(255)-per-byte on the hot path (LUT or equivalent)
- [ ] #2 iovec walk shared via one helper; var_shift/gf2p8 lane loops de-duplicated where clean
- [ ] #3 behavior unchanged: full suite green, clippy + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
