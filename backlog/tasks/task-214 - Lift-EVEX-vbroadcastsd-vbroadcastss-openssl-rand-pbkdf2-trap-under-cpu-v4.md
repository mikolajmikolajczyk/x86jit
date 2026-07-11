---
id: TASK-214
title: Lift EVEX vbroadcastsd/vbroadcastss (openssl rand/pbkdf2 trap under --cpu v4)
status: To Do
assignee: []
created_date: '2026-07-11 12:05'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 243000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found during task-128 end-to-end: 'openssl rand -hex' and 'openssl enc -pbkdf2' trap UnknownInstruction under --cpu v4 on EVEX vbroadcastsd (62 f2 fd 28 5a ..., opcode 0x5A). openssl's v4 crypto/PRNG paths hit EVEX broadcast (5a=bcast sd, 18=bcast ss, plus vpbroadcast{b,w,d,q} 78/79/58/59). Lift the EVEX broadcast family (masked/zeroing, broadcast a scalar/element across the dest, reg+mem src). Established masked-EVEX helper->interp pattern + native bit-exact. Unblocks openssl rand + any v4 binary using broadcasts. Also note: AT_RANDOM real-bytes-in-HostEntropy (task-128 leftover) needs a setup_stack API change - separate.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
