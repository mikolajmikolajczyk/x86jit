---
id: TASK-208
title: 'Lift MMX↔XMM bridge: emms/movq2dq/movdq2q (MMX aliases x87 mantissa)'
status: To Do
assignee: []
created_date: '2026-07-10 22:55'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 237000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real v4 binaries show emms 126, movq2dq 184, movdq2q 128 — unlifted. MMX registers MM0-7 alias the low 64 bits of the x87 fpr stack (physical, not top-relative). movq2dq copies an MMX reg into the low 64 of an xmm (upper zeroed); movdq2q copies xmm low 64 into an MMX reg; emms marks the x87/MMX tag word empty (in our model ~ a no-op or resets fpu_top/tags). Needs MMX↔x87 aliasing semantics in CpuState/interp before these lift cleanly. Deferred from the 2026-07-11 trap-and-fix batch (AES/SHA landed; MMX needs the aliasing design first). Differential vs Unicorn once modeled.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
