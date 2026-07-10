---
id: TASK-207
title: >-
  Lift SHA-NI: sha256rnds2/sha256msg1/sha256msg2 +
  sha1rnds4/sha1nexte/sha1msg1/sha1msg2
status: Done
assignee: []
created_date: '2026-07-10 22:36'
updated_date: '2026-07-10 22:53'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 236000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Objdump of real v4 binaries (libcrypto/openssl) shows the SHA-NI family present and unlifted: sha256rnds2 128, sha1rnds4 80, sha1msg1 64, sha256msg1/2 48 each. All UnknownInstruction. Semantics per Intel SDM / FIPS-180 (SHA-256 two-round compression with the round constants in the implicit operand; sha1rnds4 selects f() by imm8[1:0]). Host is sha_ni-capable so validate bit-exact against the real CPU (NativeOracle). Same shared-helper + helper→interp JIT pattern as AES-NI (task-205). Found via trap-and-fix recon 2026-07-11.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 SHA-256 ops lifted: sha256rnds2 (2 rounds, xmm0 = wk), sha256msg1, sha256msg2
- [x] #2 SHA-1 ops lifted: sha1rnds4 (imm-selected function), sha1nexte, sha1msg1, sha1msg2
- [x] #3 shared FIPS-180 helpers (interp==jit via helper→interp); native bit-exact (host has sha_ni)
- [x] #4 differential/jit tests per op; suite green; clippy+fmt
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Session 2026-07-11. Implemented SHA-NI (task-207) mirroring AES-NI (task-205). New x86jit-core/src/sha.rs (pure-Rust FIPS-180-4 primitives), IR ops VSha/VShaM + ShaOp enum, lift_sha, exec_v_sha(_m)/exec_sha(_mem), sha_helper/sha_mem_helper JIT. native_sha_matches_interp RAN bit-exact (host sha_ni present) + sha_all_variants_match_interp jit==interp. Full suite green, clippy+fmt clean. NOT committed — awaiting review.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
