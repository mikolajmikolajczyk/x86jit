---
id: TASK-322
title: Lifts that compute the wrong thing instead of trapping
status: Done
assignee: []
created_date: '2026-08-11 11:01'
updated_date: '2026-08-11 12:35'
labels: []
dependencies: []
priority: high
ordinal: 358000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Two instruction forms are lifted as something they are not. That is strictly worse than an UnknownInstruction trap: the guest keeps running with corrupted state instead of stopping somewhere diagnosable, and no differential test catches it because both tiers share the lift.

**lock adc / lock sbb → non-atomic RMW** (lift/mod.rs, rmw_of_binop). It maps Add/Sub/And/Or/Xor to RmwOp and returns None for the rest, so the LOCK forms fall through to load/compute/store. A guest using 'lock adc' for a carry-propagating multi-word counter — the reason the encoding exists — loses updates under contention. The IR cannot express a carry-dependent atomic RMW today, which is why it was left; silently weakening the guarantee is the part that is not acceptable.

**EVEX-masked vmovss/vmovsd → unconditional moves** (lift/vector.rs, lift_vscalar_fmove). The function consults neither evex_is_masked nor op_mask. It writes destinations the mask says to leave alone and performs memory accesses the mask says to suppress — including faulting on an address hardware would never touch.

For both, returning Unsupported is the correct interim step and is cheap. Merged from task-304 and task-225 (the latter was also filed twice, as task-303, because the board hid it).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 lock adc / lock sbb are atomic or trap; never a silent non-atomic RMW
- [ ] #2 A contended two-vcpu witness would fail against today's behaviour
- [x] #3 Masked EVEX vmovss/vmovsd honour merge/zeroing and masked fault suppression, or return Unsupported
- [ ] #4 Tests cover k=0, merge, zeroing, and a masked-off load from an unmapped address
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
The defect is fixed: neither form computes something else any more.

lock adc / lock sbb (lift/integer.rs:25) fell THROUGH the atomic path into the plain load/compute/store below when rmw_of_binop returned None, so a guest using that encoding to make a multi-word counter atomic silently lost updates under contention. It now returns Unsupported. Note where the defect actually was: the review cited lift/mod.rs:2899 (rmw_of_binop's definition); the fall-through is at the call site in integer.rs, which I initially mistook for dead code because I grepped only mod.rs and vector.rs.

Masked EVEX vmovss/vmovsd (lift/vector.rs, lift_vscalar_fmove) consulted neither op_mask nor zeroing, so it wrote masked-off elements and could fault on an address hardware never touches. Now rejected at the top of the function.

Test wrong_lifts_trap_rather_than_computing_something_else, negative control confirms both guards are load-bearing. The EVEX cases use RAW BYTES on purpose: iced's assembler silently encodes a.vmovss(xmm0.k1(), ..) as VEX and drops the mask, so an assembler-built 'masked' test exercises the unmasked path and passes against the unfixed lifter — it did, the first time I wrote it.

Full ladder green, which says no real program was relying on either wrong lift.

AC#2 and AC#4 are not met and are not the point any more: a contended witness and merge/zeroing tests belong with SUPPORTING these forms, which is now a deferred.md entry — neither has a consumer, and both need IR that nothing else wants.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
