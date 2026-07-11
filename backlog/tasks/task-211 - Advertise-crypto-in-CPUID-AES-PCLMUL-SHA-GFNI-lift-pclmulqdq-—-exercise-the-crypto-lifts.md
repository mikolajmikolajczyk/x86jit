---
id: TASK-211
title: >-
  Advertise crypto in CPUID (AES/PCLMUL/SHA/GFNI) + lift pclmulqdq — exercise
  the crypto lifts
status: Done
assignee: []
created_date: '2026-07-11 06:38'
updated_date: '2026-07-11 07:12'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 240000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
KEY FINDING (2026-07-11 PoC): the AES/SHA/GFNI lifts (task-205/207/210) are correct (native bit-exact) but DORMANT — CpuFeatures/CPUID does not advertise AES/SHA/GFNI, so real binaries (openssl, ssh) take the SOFTWARE crypto path and never execute our lifts. PoC: temporarily setting leaf1_ecx bit 25 (AES) made real 'openssl enc -aes-256-cbc' hit our aes_enc 3000+ times with ciphertext BIT-IDENTICAL to host openssl — proving end-to-end correctness through a full multi-round cipher. To make this the default (and enable ssh/TLS to exercise our crypto), advertise the features. Prereqs: lift pclmulqdq (GHASH/GCM + ssh need it; currently UnknownInstruction), and lift any wider vaes/vpclmul forms openssl's AVX paths use (VAES ymm) so 'advertise ⊆ lift' holds (the features.rs invariant + cpuid_advertises_only_what_lifts compat test). This is the payoff that turns the crypto opcode work from unit-tested into real-workload-exercised.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 lift pclmulqdq/vpclmulqdq (carry-less GF(2) multiply, imm8-selected 64x64→128); native bit-exact
- [x] #2 add Feature::{Aes,Pclmul,Sha,Gfni} + CPUID projection (leaf1_ecx bit1/25, leaf7_ebx bit29, new leaf7_ecx bit8); add to v2/v3/v4 presets per real hardware
- [x] #3 satisfy 'advertise ⊆ lift': lift any wider AES/VAES form the corpus hits (VEX.256/EVEX vaes) before advertising, or gate advertising to what lifts; compat cpuid_advertises_only_what_lifts stays green
- [x] #4 END-TO-END: real openssl aes-256-cbc/gcm + dgst-sha256 under --cpu v4 produce host-identical output AND our lifts actually fire (counter/trace); full v4 corpus stays green 3-way
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11. Lifted pclmulqdq/vpclmulqdq (VPclmul/VPclmulM IR, pclmul.rs primitive, helper→interp JIT). Advertised Feature::{Aes,Pclmul,Sha,Gfni}: leaf1_ecx bit1/25, leaf7_ebx bit29, new leaf7_ecx bit8; AES+PCLMUL in v2/v3/v4, SHA+GFNI in v4. stable()/Default UNCHANGED → corpus/compat untouched, advertise⊆lift holds (128-bit only; wide VAES/VPCLMUL leaf7_ecx 9/10 stay off so guest picks AES-NI path). Tests: pclmul KA unit + jit==interp (pclmul_all_variants) + native bit-exact (native_pclmul_matches_interp). E2E real openssl under --cpu v4 jit+interp: aes-256-cbc/gcm/dgst-sha256 all HOST-IDENTICAL; AES lift fires ~14K× (cbc/ctr/ecb), SHA fires 112×. NOTE: openssl 3.6 GCM provider uses software table-GHASH under emulation (never routes clmul), so pclmul does not fire via openssl GCM — pclmul firing proven at instruction level by native oracle. Full suite 427/427, clippy+fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
