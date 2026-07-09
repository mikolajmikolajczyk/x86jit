---
id: TASK-176
title: 'Small v3 scalars: tzcnt / lzcnt / movbe (size-parametric)'
status: To Do
assignee: []
created_date: '2026-07-09 09:04'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
ordinal: 200000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Small, clean v3 scalar lifts — warmup before the EVEX work and real gaps for v3 binaries (glibc AVX2 builds emit bare tzcnt = F3 0F BC; we lift Bsf but not Tzcnt, so a v3 binary traps). tzcnt (count trailing zeros, = bit-width on zero, sets ZF/CF unlike bsf), lzcnt (count leading zeros), movbe (byte-swapped load/store). DESIGN per conventions 'width is a field': one op per semantic with size:u8 (r32/r64 both pass it) — extend BitScan (already size-parametric) with a defined-on-zero variant for tzcnt/lzcnt rather than new per-width ops; movbe = existing Load/Store + bswap, no new op. jit==interp tested. Prereq framing: also needed for the eventual advertise-v3/v4.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 tzcnt/lzcnt lifted size-parametric (reuse BitScan seam), correct zero + flag (ZF/CF) semantics; jit==interp
- [ ] #2 movbe lifted via load/store + bswap reuse (no new IR op); jit==interp
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
