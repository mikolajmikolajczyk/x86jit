---
id: TASK-256
title: >-
  Lift VEX float cluster — vblendvps m128 src2 (Celeste blocker) + vblendps/pd,
  dppd, vdpps/vdppd imm8 blend/dot
status: In Progress
assignee: []
created_date: '2026-07-15 22:09'
updated_date: '2026-07-15 22:36'
labels: []
dependencies: []
ordinal: 286000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Celeste (FNA/MonoGame, AVX-heavy) faulted on c4 e3 59 4a 1d ... = vblendvps xmm3, xmm4, [rip+disp32], xmm3 — the VEX variable-blend with an m128 second source, which lift_vblendv explicitly defers (mem src2 deferred). Lift that memory-source form (primary blocker), plus the cheap adjacent VEX/SSE imm8-blend + dot-product cluster whose SSE base is (or can be) a mechanical reuse: vblendps/vblendpd (+ SSE blendps/blendpd), dppd (SSE) + vdpps/vdppd (VEX 3-operand). Mirror the task-255 vinsertps pattern: aliasing-safe explicit-merge-base IR, VZeroUpper for VEX.128, vex_eq_sse + native_*_matches_interp differential tests, three tiers (decode/interp/cranelift).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 vblendvps/vblendvpd/vpblendvb with m128 src2 lifted (all three tiers); Celeste wild bytes c4 e3 59 4a decode+run test asserting exact shape
- [x] #2 imm8 blend lifted: SSE blendps/blendpd + VEX vblendps/vblendpd (3-operand, upper-zero)
- [x] #3 dot product lifted: SSE dppd + VEX vdpps/vdppd (3-operand, upper-zero)
- [x] #4 each op: vex_eq_sse (or unicorn) differential + native_*_matches_interp bit-exact AVX oracle + jit_eq_interp; dst==src2/dst==mask aliasing + 255:128 upper-zero covered
- [x] #5 cargo test workspace green; clippy -D warnings clean; fmt; compat coverage.json/isa-coverage.md + coverage_ratchet allowlist updated
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
