---
id: TASK-205
title: >-
  Lift AES-NI: aesenc/aesdec/aesenclast/aesdeclast/aesimc/aeskeygenassist (+VEX
  vaes*)
status: Done
assignee: []
created_date: '2026-07-10 22:02'
updated_date: '2026-07-10 22:35'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 234000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Objdump of real v4 binaries (libcrypto/libssl/openssl) shows AES-NI is by far the most-present unlifted opcode group: vaesenc 16477 occurrences, aesenc 550, aesdec 346, aesenclast 92, aesdeclast 59, aeskeygenassist 31, aesimc 3. Currently all UnknownInstruction (cold in the runs swept — glibc IFUNC/openssl picks them on the AES path, hit by real TLS/crypto workloads). Semantics fully specified by FIPS-197 (SubBytes/ShiftRows/MixColumns/AddRoundKey); implement a shared AES round helper (S-box + GF(2^8) mix) used by interp and a JIT helper, validated bit-exact against the real CPU (NativeOracle, host is x86-64-v4 with AES-NI). High value: unblocks running real crypto/TLS binaries under v4. Found via the trap-and-fix/coverage recon (2026-07-11).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 SSE AES-NI lifted: aesenc/aesdec/aesenclast/aesdeclast (round + last-round), aesimc (inverse mix-columns), aeskeygenassist
- [x] #2 VEX forms (vaesenc/vaesdec/vaesenclast/vaesdeclast) lifted with 255:128 zeroing
- [x] #3 interp + jit (interp==jit) via shared FIPS-197 round helper; native cross-check bit-exact (host has AES-NI)
- [x] #4 differential/jit tests per op; compat regenerated; suite green; clippy+fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
