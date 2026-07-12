---
id: TASK-236
title: 'perf: audit + rank the ~223 helper->interp fallbacks by games-hotness'
status: To Do
assignee: []
created_date: '2026-07-12 20:21'
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
- [ ] #1 a ranked list of helper->interp SIMD ops (hot-first) with native-lowerability verdict per op, committed under backlog/docs
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
