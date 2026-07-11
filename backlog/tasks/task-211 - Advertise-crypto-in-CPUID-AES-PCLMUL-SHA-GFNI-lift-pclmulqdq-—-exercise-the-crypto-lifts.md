---
id: TASK-211
title: >-
  Advertise crypto in CPUID (AES/PCLMUL/SHA/GFNI) + lift pclmulqdq — exercise
  the crypto lifts
status: To Do
assignee: []
created_date: '2026-07-11 06:38'
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
- [ ] #1 lift pclmulqdq/vpclmulqdq (carry-less GF(2) multiply, imm8-selected 64x64→128); native bit-exact
- [ ] #2 add Feature::{Aes,Pclmul,Sha,Gfni} + CPUID projection (leaf1_ecx bit1/25, leaf7_ebx bit29, new leaf7_ecx bit8); add to v2/v3/v4 presets per real hardware
- [ ] #3 satisfy 'advertise ⊆ lift': lift any wider AES/VAES form the corpus hits (VEX.256/EVEX vaes) before advertising, or gate advertising to what lifts; compat cpuid_advertises_only_what_lifts stays green
- [ ] #4 END-TO-END: real openssl aes-256-cbc/gcm + dgst-sha256 under --cpu v4 produce host-identical output AND our lifts actually fire (counter/trace); full v4 corpus stays green 3-way
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
