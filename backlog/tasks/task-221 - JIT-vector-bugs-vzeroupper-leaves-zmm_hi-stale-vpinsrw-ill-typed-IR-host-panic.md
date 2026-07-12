---
id: TASK-221
title: >-
  JIT vector bugs: vzeroupper leaves zmm_hi stale + vpinsrw ill-typed IR
  host-panic
status: Done
assignee: []
created_date: '2026-07-12 08:07'
updated_date: '2026-07-12 08:35'
labels:
  - 'crate:cranelift'
  - bug
  - code-review
dependencies: []
ordinal: 250000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable whole-codebase review. TWO bugs, both in the Cranelift vector codegen (x86jit-cranelift/src/codegen/vector.rs), interp side possibly in x86jit-core/src/interp/vector.rs. (1) CRITICAL: emit_v_zero_upper_all clears only ymm_hi (store_ymm_hi_zero) for regs 0..16 but NEVER clears zmm_hi (bits 511:256). Under AVX-512, vzeroupper/vzeroall must zero bits above 128 for regs 0-15 — the JIT leaves zmm_hi stale, so interp != JIT after a vzeroupper when zmm uppers were live. Confirmed: the fn has no zmm_hi store. Check what the interpreter's exec_v_zero_upper_all does and make the JIT match it (zero zmm_hi[0..16] too; verify the reg range — vzeroupper affects regs 0-15's full width above xmm). (2) HIGH: emit_v_insert_lane has size arms 1=>I8X16, 4=>I32X4, _=>I64X2 — MISSING 2=>I16X8. A vpinsrw/pinsrw (size 2) falls to I64X2 and does insertlane on a 2-lane vector with index %(16/2)=up to 7 -> Cranelift verifier rejects -> HOST PANIC on tier-up. Add the 2=>I16X8 arm. Interp handles size 2 correctly. Add jit==interp coverage for vpinsrw and a vzeroupper-with-live-zmm-upper case.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 JIT vzeroupper/vzeroall zeroes zmm_hi for the affected regs; jit==interp on a snippet with live zmm uppers then vzeroupper
- [ ] #2 vpinsrw/pinsrw (size 2) compiles (I16X8) and matches interp; no verifier panic on tier-up
- [ ] #3 cargo nextest (--features unicorn, minus fuzz_robustness) green; clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
