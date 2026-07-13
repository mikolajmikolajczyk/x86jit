---
id: TASK-236
title: 'perf: audit + rank the ~223 helper->interp fallbacks by games-hotness'
status: Done
assignee: []
created_date: '2026-07-12 20:21'
updated_date: '2026-07-13 08:20'
labels:
  - 'crate:cranelift'
  - 'goal:perf'
milestone: ps4-perf
dependencies: []
ordinal: 265000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tier-1 (doc-33). x86jit-cranelift/src/lib.rs has ~223 helper-call sites; many SIMD ops fall back to an interpreter helper (a C-ABI call out of JIT per instruction) instead of native Cranelift/NEON lowering — a hot-path perf killer for SIMD-heavy game code. Audit every helper->interp SIMD fallback, classify (legit: cpuid/x87/gather/rare vs replaceable: hot AVX/SSE float+integer), and RANK by games-hotness (vmulps/vaddps/vfmadd/vshufps/vblend/vcvt* etc.). Output a ranked worklist feeding the native-lowering task. No behavior change — measurement/analysis only.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 a ranked list of helper->interp SIMD ops (hot-first) with native-lowerability verdict per op, committed under backlog/docs
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. Audit doc: backlog/docs/doc-34. Two cross-verified inventory passes (native map + helper map) over codegen/{mod,vector}.rs + lift/interp + coverage.json. KEY FINDINGS (reshape doc-33 Tier-1): (1) game-hot float core is ALREADY native (add/sub/mul/div/min/max/sqrt/cmp + all packed-int + bitwise+ternlog + imm-shifts + common shuffles/blends/broadcasts/pshufb) -> task-237's 'native-lower vmulps/vaddps, 2-10x' is a no-op, reset expectation. (2) HIGHEST-VALUE GAP is a correctness hole not a lowering: packed float<->int converts (cvtps2dq/cvtdq2ps/cvttps2dq/cvtps2pd/cvtpd2ps) are UNIMPLEMENTED and TRAP -- in x86-64-v1(SSE2) missing list, universal + very hot in games -> filed as top ps4-perf task. (3) PS4=Jaguar=SSE4.2+AVX128, NO AVX2/FMA3/AVX512 -> most remaining helper ops (var-shift, cross-lane permute, EVEX-masked, even FMA) unreachable by PS4 guests. Ranked worklist in doc: #1 cvt-packed(MISSING,do-first), #2 shift_reg, #3 dpps (SSE, PS4-reachable), #6 FMA(general track not PS4). LEGIT-keep: crypto/string/maskmov/mmx-bridge. DoD: doc-only, no code touched since task-235 green (nextest 662 passed / clippy / fmt).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
