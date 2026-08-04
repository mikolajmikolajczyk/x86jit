---
id: TASK-231
title: 'lift: x87 fnstenv — a second guest fault on LN''s thread, same run'
status: Done
assignee: []
created_date: '2026-08-02 16:01'
updated_date: '2026-08-02 18:45'
labels:
 - lift
 - x87
dependencies: []
ordinal: 327000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Second unimplemented lift found in the same Little Nightmares run as the vextractf128 gap, on another thread:

 fnstenv -0x28(%rbp)
 faulting bytes: [d9 75 d8]

x87 environment store. Whether the emulator needs the full 28-byte environment or only the fields the guest then reads is worth establishing from the caller before implementing all of it — the surrounding code is the thing to read first.

Filed separately from the vextractf128 task because the two are unrelated instruction families and one may be needed without the other.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 fnstenv lifts, or the fields the guest actually consumes are named and the rest refused explicitly rather than silently zeroed
- [x] #2 differential-tested against the existing oracle
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented 2026-08-02. Chose option (b): the fields the guest consumes are modeled, the rest is documented as unmodeled rather than silently passed off as real. Caveat stated honestly - a single 28-byte store has no sub-instruction granularity, so 'refuse the rest' is not literally implementable without refusing the whole instruction. What is modeled: control word exact; status word TOP (C0-C3 and the exception flags are a pre-existing model gap, now named in the code); tag word DERIVED PER SDM from the live fpr[] bytes (00 valid / 01 zero / 10 special), not zeroed - 11/empty is never produced because this FPU models no stack-emptiness bit, the same simplification exec_fxstate already makes. FIP/CS+FOP/FDP/FDS (offsets 12..28) are not modeled; the mitigation for 'refuse loudly' is that fldenv/fnsave/frstor stay UNLIFTED, so a guest that round-trips the environment traps instead of restoring a fabricated one. The 14-byte form (66 D9 /6) is refused outright with LiftError::Unsupported - different field layout. FNSTENV's SDM-mandated side effect (mask all six FP exceptions after the store) is implemented. Layout was measured on real silicon, not guessed: reserved upper halves at offsets 2/6/10/26 are written 0xFFFF, not zero. Zero JIT plumbing - routes through the existing x87 helper; the store goes through FpMem::store -> write_ram_guest so note_write sees it. Deliberate divergences from Unicorn, all measured side by side: (1) reserved halves - we write 0xFFFF matching silicon, Unicorn writes 0x0000, we are right, excluded from the compare; (2) exception masking - QEMU omits it, we are right, asserted against the SDM instead of differentially; (3) FIP/FDP - Unicorn tracks them, we do not, the documented gap, excluded. Control/status/tag words compare EXACTLY against Unicorn and agree. Tests: lift::tests::fnstenv_lifts_only_the_28_byte_form (the literal faulting bytes d9 75 d8), differential x87_fnstenv_env28_matches_unicorn + x87_fnstenv_masks_exceptions (both RAN, not skipped), jit x87_fnstenv_match_interp. compat artifact unchanged - probe_code(Fnstenv_m28byte) returns Unencodable like the already-lifted Fnstcw/Fldcw, so ratchet ALLOWLIST entries would fail coverage_lists_have_no_stale_entries; matches existing precedent. Gates on the merged main tree: 916 passed / 8 skipped, clippy clean, fmt clean, aarch64 check clean. NOT COMMITTED. Follow-up filed: fldenv is the likely next trap on the same LN thread.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
