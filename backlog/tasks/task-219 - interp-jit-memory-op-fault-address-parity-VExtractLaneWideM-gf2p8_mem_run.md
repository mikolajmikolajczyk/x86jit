---
id: TASK-219
title: 'interp/jit: memory-op fault-address parity (VExtractLaneWideM, gf2p8_mem_run)'
status: Done
assignee: []
created_date: '2026-07-12 07:17'
updated_date: '2026-07-12 07:35'
labels:
  - 'crate:core'
  - 'crate:cranelift'
  - code-review
dependencies: []
ordinal: 248000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Code-review finding on task-215. Two new memory ops report a different fault address between the interpreter and the JIT — invisible to jit==interp differential tests only because they don't fault, but the lockstep interp-vs-hardware tracer and any SIGSEGV-inspecting guest would see the divergence. (1) VExtractLaneWideM: interp (exec_v_extract_lane_wide_m) stores lane-by-lane and can commit lane 0 before faulting on lane 1 at addr+16, while the JIT (emit_v_extract_lane_wide_m) does one up-front checked_addr(a, n*16) and traps at base a with nothing written. (2) gf2p8_mem_run reports the faulting address as the 128-bit lane base cea even when only the high 8-byte half (cea+8) is unmapped. Make the two backends agree: pick one policy (recommend the JIT's up-front whole-region check, since a partial vector store is not architecturally observable mid-op) and align interp to it, OR make both report the exact faulting sub-address. Keep it consistent and documented.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 interp and JIT agree on committed memory + reported fault address for a boundary-straddling VExtractLaneWideM store
- [ ] #2 gf2p8_mem_run fault address matches the JIT path for a half-unmapped matrix load
- [ ] #3 cargo nextest (--features unicorn, minus fuzz_robustness) green; clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
