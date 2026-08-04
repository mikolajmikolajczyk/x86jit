---
id: TASK-232
title: 'lift: x87 fldenv/fnsave/frstor — the likely next trap on LN''s x87 thread'
status: Done
assignee: []
created_date: '2026-08-02 18:42'
updated_date: '2026-08-02 22:34'
labels:
 - lift
 - x87
dependencies: []
ordinal: 328000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Follow-up to TASK-231 (fnstenv). FreeBSD's fenv.h — which PS4 titles link — always pairs fnstenv with fldenv: fegetenv does fnstenv then fldcw, and feclearexcept/fesetenv/feupdateenv use fldenv proper. TASK-231 deliberately left fldenv, fnsave and frstor UNLIFTED so that a guest round-tripping the x87 environment traps loudly rather than restoring the partly-fabricated image fnstenv writes (FIP/CS+FOP/FDP/FDS are not modeled). That is the right call, but it means Little Nightmares is expected to fault again on the same thread as soon as it hits the restore half of the pair.

Doing this properly means deciding what fldenv should do with the environment fields we do not model. Options worth weighing before writing code: ignore the pointer block on load (consistent with never having produced a real one), or model FIP/FDP for real — the latter needs the last x87 opcode, which is not available at the helper, and note that modern CPUs only update FDP when an unmasked FP exception is pending (CPUID.07H:EBX[6], FDP_EXCPTN_ONLY) and we never raise one.

Also worth settling here: the status-word exception flags (C0-C3, PE/UE/OE/ZE/DE/IE) that TASK-231 names as a pre-existing model gap. fldenv loading flags we never set, and fnstenv storing flags we never computed, are the same gap seen from both ends.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 fldenv lifts, with an explicit decision recorded for the environment fields x86jit does not model
- [x] #2 fnsave/frstor either lift or stay refused deliberately, with the reason stated rather than left implicit
- [x] #3 differential-tested against Unicorn, with any deliberate divergence measured and named the way TASK-231 did
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
IMPLEMENTED 2026-08-03. fldenv lifts; fnsave/frstor stay deliberately refused.

PER-FIELD DECISION, recorded on x87::load_env28 as the mirror of env28:
- Control word (offset 0): HONORED IN FULL, the same assignment fldcw makes. It carries the rounding control x87::rc reads for fist/fistp, so dropping it would be wrong arithmetic with no trap - and putting it back is the entire reason FreeBSD's fenv restore exists around powf/expf.
- Status word (4): TOP only, into fpu_top. C0-C3 and the exception flags are modeled nowhere, so they are dropped rather than parked where nothing reads them.
- Tag word (8): IGNORED. Tags are DERIVED from the live fpr[] bytes at every store (tag_word, and the abridged FTW in exec_fxstate) - there is no tag state to load into, and fldenv may not touch fpr[] to manufacture one. The single tag a loaded word could carry that derivation cannot (11 = empty) is exactly the stack-emptiness bit this FPU does not model.
- FIP / CS+FOP / FDP / FDS (12/16/20/24): IGNORED, symmetric with env28 never having produced a real one, so the round trip loses nothing observable.

EXCEPTION UNMASKING: a loaded CW can unmask an exception whose flag is set in the loaded SW, and hardware would raise it on the next FP instruction. This FPU raises no FP exception and tracks no exception flag, so the mask bits are stored verbatim (a later fnstcw/fnstenv reads them back) and NOTHING is raised. Coherent with the unmodeled MXCSR on the SSE half of the very same fenv_t (task-82: ldmxcsr is a no-op, stmxcsr a constant 0x1F80).

SDM points held: fldenv touches NO data register (that is frstor), clears nothing (that is fnclex), and the 14-byte form (66 D9 /4) is refused with LiftError::Unsupported, symmetric with fnstenv.

fnsave/frstor REFUSED, deliberately: they move the eight data registers, and the image holds those in STACK order ST(0)..ST(7) while fpr[] is indexed physically and the only other register-image path here (exec_fxstate) copies it physically. Picking a convention is a register-file question with its own differential surface, not a consequence of the environment decision. Stated in code and pinned by a negative test.

SITE PROVEN UNBLOCKED, not merely advanced: lift::tests::fldenv_and_the_fenv_restore_block_around_it_lift lifts the exact 16 faulting guest bytes as ONE block (d9 65 d8 / 0f ae 5d f4 / c4 e2 40 f2 4d f4 / 09 d1). Since a single unsupported instruction fails the whole block, that is the proof the trap does not just move one instruction along.

CONTROL WORD PROVEN HONORED END-TO-END. The test performs the guest's actual round trip - fnstenv, patch ONLY the saved control word, fldenv - with NO fldcw anywhere in the body, so fldenv is the sole path from the patched image back into the FPU. Then fistp of 0.75 and -0.75 under each RC; the pair separates all four modes: nearest (1,-1), down (0,-1), up (1,0), truncate (0,0). Re-verified independently on main by mutation: deleting the cpu.fpu_cw assignment in load_env28 fails BOTH tests at RC=down with left (1,-1) vs right (0,-1) - i.e. still rounding to nearest. Restored, green again.

Tests assert ExitKind::Hlt on both engines, because a jit_eq_interp parity check alone cannot prove a lift exists (an unlifted opcode traps identically in both tiers - observed on task-235).

NO divergence from Unicorn: the image fields the two engines disagree on (the 0xFFFF reserved halves, the FIP/FDP block) are precisely the ones fldenv discards, so nothing is left to diverge on. Control word after restore 0x0F7F, status word 0x2800, all eight rounding results agree exactly.

Compat: no diff, as predicted - single-memory-operand x87 shapes land in the unencodable bucket. No ratchet entry needed or attempted.

Gates on the merged main tree: 924 passed / 8 skipped, clippy clean, fmt clean, aarch64 check clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
