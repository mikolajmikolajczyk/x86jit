---
id: TASK-224
title: 'flag-elision: shift-by-0 must preserve flags (both engines wrong vs hardware)'
status: Done
assignee: []
created_date: '2026-07-12 08:07'
updated_date: '2026-07-12 09:09'
labels:
  - 'crate:core'
  - bug
  - code-review
dependencies: []
ordinal: 253000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable whole-codebase review, CRITICAL and DELICATE (touches shared flag infrastructure — do this SOLO, not in parallel with the other lifter tasks). x86 variable-count shifts (shl/shr/sar/rol/ror by CL) with count == 0 leave ALL flags UNCHANGED. Currently both the interpreter and the JIT compute/write the shift's flags unconditionally, so when a runtime count is 0 the flags are wrong vs hardware — and because BOTH engines are wrong the SAME way, interp==jit differential testing is blind to it (only a hardware oracle / the lockstep tracer catches it). The flag-elision pass (lift/mod.rs elide_dead_flags) interacts: it may mark the shift's flags dead based on a static assumption they are always written. Fix: the count-conditional-flag semantics — when the shift count is 0 at runtime, do not update CF/OF/SF/ZF/AF/PF (leave the prior flags). This likely needs a runtime count check in the shift flag path (interp and codegen) and care that elide_dead_flags does not incorrectly elide a live prior flag producer across a possibly-no-op shift. VALIDATE with the lockstep interp-vs-hardware tracer (scripts/lockstep.sh) or a native-oracle test, since the differential suite cannot see this class.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 a shl/shr/sar/rol/ror reg,cl with cl==0 leaves all flags unchanged, in interp AND jit, matching hardware
- [ ] #2 elide_dead_flags does not drop a live flag producer across a count-conditional shift
- [ ] #3 validated against a hardware oracle (lockstep tracer or native asm test), not just interp==jit
- [ ] #4 cargo nextest (--features unicorn, minus fuzz_robustness) green; clippy -D warnings + fmt clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
