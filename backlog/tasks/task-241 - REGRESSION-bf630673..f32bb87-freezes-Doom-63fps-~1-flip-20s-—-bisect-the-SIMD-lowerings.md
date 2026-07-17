---
id: TASK-241
title: >-
  REGRESSION: bf630673..f32bb87 freezes Doom (63fps -> ~1 flip/20s) — bisect the
  SIMD lowerings
status: Done
assignee: []
created_date: '2026-07-13 11:58'
updated_date: '2026-07-13 12:30'
labels:
  - regression
  - simd
  - perf
  - real-software
  - doom
dependencies: []
ordinal: 270000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Bumping unemups4's x86jit pin from bf630673 to f32bb87 catastrophically regresses the Doom (doomgeneric) homebrew running on the unemups4 PS4 emulator. A/B measured via flips/frames rendered in a fixed 20s wall-clock window (SubmitFlip count): at bf630673 Doom runs ~63fps (1262 flips/20s, fully playable — menu, gameplay); at f32bb87 it drops to ~1 flip/20s = effectively frozen. The near-total freeze (not a gradual slowdown) points to a SIMD CORRECTNESS regression: a wrong result on an op Doom's software renderer runs every frame likely traps the guest in an infinite/near-infinite loop rather than a mere perf cliff. The 60-commit range bf630673..f32bb87 includes threading/linux work AND SIMD lifts; strongest suspects are the SIMD lowerings the Doom renderer exercises per frame: task-237 (native-lower register-count packed shifts ff4f471; native-lower dpps a7e25a3), task-239 (packed float<->int converts cvt* d9ef82f), task-195 (insertps/dpps/pcmpistrm 36e0647), task-190 (SSE2 packed sat add/sub, pavg, signed packs, pmaddwd e97e83e), and the task-215 SIMD lifts. Doom uses packed int (pmaddwd/packs/pavg), packed shifts, and float<->fixed cvt heavily. Bisect with the interp-vs-hardware differential tracer (task-215 tooling) + the task-235 game-shaped microbench — a lowering whose native path disagrees with interp/hardware is the culprit. Note: movmskps/pd (task-240 f32bb87) is a NEW lift and unlikely to freeze; the regression is almost certainly a task-237/239/190/195/215 native-lowering.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Root-cause commit in bf630673..f32bb87 identified (bisect); the failing SIMD op named with the wrong-result or perf-cliff evidence (interp-vs-hardware diff or microbench)
- [ ] #2 Fix lands so a build carrying the MOVMSKPS/MOVMSKPD lift (task-240) also runs Doom at ~full speed in unemups4 (>~50fps, i.e. hundreds of flips/20s)
- [ ] #3 A regression test (differential or microbench) covers the specific op so this class doesn't silently recur
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Resolution — NOT an x86jit bug

Bisected to x86jit commit `35abc06` (task-222, "SYSCALL RCX/R11 (amd64-only)"). That is a CORRECTNESS fix — real SYSCALL clobbers RCX(<-RIP)/R11(<-RFLAGS). The unemups4 emulator relied on the old lift preserving RCX (it read a syscall 4th arg from RCX). The freeze is unemups4-side and fixed there (unemups4 task-106: move the 4th arg via R10). No x86jit change needed. My earlier SIMD suspicion was wrong. Can be closed once unemups4 lands task-106 + re-bumps.
