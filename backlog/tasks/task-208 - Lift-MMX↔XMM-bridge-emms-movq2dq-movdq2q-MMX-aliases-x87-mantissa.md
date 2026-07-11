---
id: TASK-208
title: 'Lift MMX↔XMM bridge: emms/movq2dq/movdq2q (MMX aliases x87 mantissa)'
status: Done
assignee: []
created_date: '2026-07-10 22:55'
updated_date: '2026-07-11 10:04'
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
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11. MMX↔x87 bridge: movq2dq/movdq2q/emms. Maintainer chose TRUE aliasing. Refactored CpuState.fpr [F80;8]->[[u8;10];8] (raw = source of truth; F80 decoded transiently at x87 stack boundary — round-trip exact for normal floats, but F80 IS lossy for NaN/MMX payloads so raw was necessary). MMX = low64 of PHYSICAL fpr[i]. movdq2q sets exp bytes 0xFFFF (Intel SDM). Blast radius small: state.rs, vm.rs accessors, x87.rs push/pop/st/set_st+fxstate. New mmx_bridge JIT helper (op 0/1); aarch64 stub updated + verified. Unicorn quirk: QEMU movdq2q leaves exp=0000 (inaccurate); kept HW-accurate 0xFFFF + custom differential mmx_bridge_matches_unicorn comparing xmm round-trip + MMX mantissa exactly, excluding exp tag bytes. All 17 x87/f80 tests green post-refactor (behavior-preserving). Suite 546/546 (--features unicorn), clippy+fmt clean, aarch64 clean. Ratchet: Emms/Movdq2q/Movq2dq allowlisted.
<!-- SECTION:NOTES:END -->
