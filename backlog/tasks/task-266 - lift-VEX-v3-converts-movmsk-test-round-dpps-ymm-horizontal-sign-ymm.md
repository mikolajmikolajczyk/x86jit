---
id: TASK-266
title: 'lift: VEX v3 converts + movmsk/test/round/dpps ymm + horizontal/sign ymm'
status: In Progress
assignee: []
created_date: '2026-07-16 14:15'
updated_date: '2026-07-16 15:06'
labels: []
dependencies: []
ordinal: 284000
---

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
task-263 work. Groups A-D lifted across lift/interp/cranelift + jit==interp and native-oracle tests. Deferred: same-width dq2ps/ps2dq ymm (other sweep); vcvtps2ph mem-dest (no scratch-vec allocator); vmpsadbw ymm mem-src; pcmpestr64 pathological 64-bit RAX/RDX length edge (clamps to <=16). Compat map intentionally not regenerated.
<!-- SECTION:NOTES:END -->
