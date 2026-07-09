---
id: TASK-179
title: >-
  CR — cranelift dedup: sig/import boilerplate, out-slot + trap-tail folds,
  emit_vlogic reuse
status: Done
assignee: []
created_date: '2026-07-09 09:56'
updated_date: '2026-07-09 10:29'
labels:
  - CR
dependencies: []
ordinal: 203000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Code-quality consolidation in x86jit-cranelift/src/{codegen.rs,lib.rs}. No behavior change (same emitted code). (H) mk_cpu_sig/vmaskmov_sig/bmi_sig hand-roll make_signature+push loop -> generalize existing params() to params(n, ret). (I) Helpers import block repeats (import_signature(sig), fn as *const u8 as u64) 9x -> local closure. (J) out-slot pattern (StackSlot alloc + stack_addr + call + stack_load x2) duplicated in emit_div and Bmi arm -> call_out2 helper. (K) X87/FxState/RepString share identical trap-check tail (icmp RET_UNMAPPED -> begin_trap_fork -> ret_no_flush -> switch) -> helper_may_trap. (L) VLogic/VLogicM inline the 4-way Xor/And/Or/Andn match that emit_vlogic already implements -> call it. (M) VLoadWide/VMovWide zero-above tail duplicated -> store_lanes_zeroed_above. Verify: build + nextest + clippy --all-targets --all-features -D warnings.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 params(n,ret) folds the 4 sig builders,helper import uses a closure,call_out2 shared by div+bmi,helper_may_trap folds the 3 trap tails,VLogic/VLogicM call emit_vlogic,store_lanes_zeroed_above shared,full suite green + clippy clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
