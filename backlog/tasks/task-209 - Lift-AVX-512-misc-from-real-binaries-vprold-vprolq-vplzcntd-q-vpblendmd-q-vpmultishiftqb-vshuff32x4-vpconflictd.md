---
id: TASK-209
title: >-
  Lift AVX-512 misc from real binaries: vprold/vprolq, vplzcntd/q, vpblendmd/q,
  vpmultishiftqb, vshuff32x4, vpconflictd
status: Done
assignee: []
created_date: '2026-07-10 22:55'
updated_date: '2026-07-11 07:39'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 238000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Objdump of real v4 binaries shows these EVEX ops present + unlifted: vprold 157, vplzcntd 156, vpmultishiftqb 124 (VBMI), vpblendmd 115, vshuff32x4, vpconflictd (host has avx512cd+avx512vbmi). Rotates (vprold/q), leading-zero-count (vplzcntd/q), mask-blend (vpblendmd/q), multishift (vpmultishiftqb), 128-bit-lane shuffle (vshuff32x4), conflict-detect (vpconflictd). Lift via the established EVEX helper→interp pattern; native bit-exact (host is v4 with these features). Found via 2026-07-11 trap-and-fix recon; batched after AES/SHA.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11. Lifted 13 EVEX mnemonics via 4 new IR ops (helper→interp, all masked/zeroing, native bit-exact): VpUnaryLane{Lzcnt,Rol,Conflict} → vplzcntd/q+vprold/q+vpconflictd/q; VpBlendm → vpblendmd/q (opmask=blend control); VShuffLane → vshuff32x4/64x2 (128-bit lane select); VpMultishift → vpmultishiftqb (VBMI, per-qword byte gather). Each: ir.rs op + lift/vector.rs + lift/mod.rs dispatch + interp exec (reusing write_masked/vec_lanes/get_velem) + JIT helper (10 or 8 registration sites) + jit==interp (jit.rs) + native bit-exact (native.rs, host has avx512cd+vbmi). ALLOWLIST: 10 in-scope AVX512F/CD mnemonics added (vpmultishiftqb stays out — VBMI unmapped in feature_gen). coverage.json regen: v4 lifted 391→409. Full suite 435/435, clippy+fmt clean.
<!-- SECTION:NOTES:END -->
