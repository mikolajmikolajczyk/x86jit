---
id: TASK-214
title: Lift EVEX vbroadcastsd/vbroadcastss (openssl rand/pbkdf2 trap under --cpu v4)
status: Done
assignee: []
created_date: '2026-07-11 12:05'
updated_date: '2026-07-11 12:22'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11. Lifted EVEX lane-broadcast family vbroadcast{i,f}{32x2,32x4,32x8,64x2,64x4,128} + scalar vbroadcastss/sd. Root cause of openssl-rand trap was vbroadcasti64x2 (128-bit chunk replicated across lanes), NOT scalar broadcast. New VBroadcastLane/VBroadcastLaneM IR (chunk 8/16/32, elem=mask granularity 4/8) via helper->interp (reg + fault-capable mem, like fma_mem). Shared broadcast_lane_lanes core. Native bit-exact (native_broadcast_lane_matches_interp, mem-source since iced only assembles mem form; reg shares core) + jit==interp (broadcast_lane_variants_match_interp). BONUS: unblocked openssl rand -> proved task-128 entropy END-TO-END: deterministic reproduces byte-identical, --entropy host differs. Ratchet: 4 mnemonics allowlisted. New Helpers fields broadcast_lane/_mem, aarch64 stub OK. Suite green, clippy+fmt clean. NOTE: reg-source lane broadcast lifts but can't be iced-assembled for a test (shares core with mem).
<!-- SECTION:NOTES:END -->
