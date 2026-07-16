---
id: TASK-260
title: >-
  AVX2 v3 sweep A — packed-integer VEX:
  saturating/avg/min-max/mulhrsw/pmaddwd/pmaddubsw (xmm+ymm)
status: To Do
assignee: []
created_date: '2026-07-16 14:11'
labels: []
milestone: open-backlog
dependencies: []
ordinal: 290000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fill missing x86-64-v3 packed-integer VEX forms via the existing lift_vpacked_bin_avx + PackedBinOp primitive. Mnemonic arms (reuse existing PackedBinOp): Vpaddsb/w, Vpaddusb/w, Vpsubsb/w, Vpsubusb/w, Vpavgb/w, Vpmaxsb/sw/uw, Vpminsb/sw/uw. New: PackedBinOp::MulHiRoundedS16 (Vpmulhrsw), widen Pmaddwd to ymm (Vpmaddwd), add Vpmaddubsw. All three tiers, jit==interp via shared primitive, native-oracle + jit tests mirroring task-259. Owns: PackedBinOp enum, lift_vpacked_bin_avx arms, pmaddwd/pmaddubsw/pmulhrsw. Does NOT touch lift_vhint/lift_vpsign (that is sweep D).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 All listed forms lift 3 tiers; jit==interp + native oracle green
- [ ] #2 clippy -D + fmt clean; compat map shows the forms lifted
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
