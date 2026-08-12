---
id: TASK-324
title: 'x87/F80 fidelity: control word, environment, and special encodings'
status: Done
assignee: []
created_date: '2026-08-11 11:02'
updated_date: '2026-08-12 08:49'
labels: []
dependencies: []
priority: high
ordinal: 360000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The x87 model stores architectural state faithfully and then does not let it govern execution. Four findings, one shape.

**The control word does not reach arithmetic** (x87.rs ~451). RC is passed only to integer conversion; every add/sub/mul/div calls F80 fixed at nearest-even with a 64-bit significand, consulting neither RC nor PC. So fldcw and fldenv visibly restore a control word and the arithmetic that follows behaves as if they had not — a guest selecting round-toward-zero or 24/53-bit precision, which is what fldcw is for, gets silently wrong results and no trap.

**fldenv/fnstenv does not round-trip** (x87.rs ~423). load_env28 keeps the control word and TOP and discards exception flags, condition codes, the tag word, FIP/FDP, selectors and opcode; env28 regenerates zeros or derives the tag from live values. FreeBSD's fenv.h round-trip around powf/expf — the sequence that made this instruction necessary — is exactly this pattern.

**The tag word cannot say 'empty'** (was task-237). Tags are derived from live fpr[] bytes, so 11 is unrepresentable: measured, fninit;fnstenv gives 0x5555 where hardware gives 0xffff. Pinned by a test that asserts the divergent values on purpose and must fail when this lands.

**F80 mis-classifies unnormals and destroys NaN identity** (f80.rs ~88). from_bytes calls any nonzero, non-max exponent Normal without checking the explicit integer bit, so an unnormal enters ordinary finite arithmetic; and every NaN arithmetic arm returns F80::nan(), discarding sign, payload and signaling/quiet status. Both contradict the bit-identical-80-bit claim.

Rounding itself is separate and stays its own task — it is measured, has exact bytes, and is a different kind of defect. Merged from task-314, 315, 237, 316.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 RC and PC reach every arithmetic operation and change its result; tested under all four rounding modes and all three precisions against hardware
- [x] #2 An arbitrary 28-byte environment image survives fldenv then fnstenv unchanged, and restored exception flags affect later execution
- [x] #3 The tag word reports 11 for empty; x87_tag_word_after_fninit_diverges_from_hardware is updated to assert the correct values
- [x] #4 Unsupported, signaling-NaN and quiet-NaN are distinct classes with x87 invalid-operation and NaN-selection rules
- [x] #5 Raw 10-byte witnesses cover pseudo-denormals, unnormals, both NaN kinds, signs and payloads
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE. Four acceptance criteria met; the fifth is met as far as state goes and its behavioural half is now TASK-328.

AC#4/#5 (fb92009) - F80 classification and NaN identity. from_bytes called every non-zero, non-max exponent a Normal without looking at the explicit integer bit, so an unnormal entered ordinary arithmetic and produced a plausible finite number; and every NaN arm of every operation returned a bare F80::nan(), discarding sign, payload and quiet status. Now follows SDM Vol 1 sec 8.2.2 Table 8-3 (integer bit separates supported from unsupported; pseudo-denormals are supported, everything else with a clear integer bit is not) and Table 4-8's X87 rows for NaN selection - which are NOT the SSE rows implemented for sse_binop_f32: x87 prefers the QNaN over an SNaN and takes the larger significand, SSE takes the first operand.

Two things the hardware witnesses corrected that the SDM text does not state: fld m80 / fstp m80 are MOVES, not conversions (an unnormal and a pseudo-denormal land in the register verbatim; going through F80 renormalized the pseudo-denormal's exponent), and fsub's negation of its subtrahend must not reach a NaN (same shape as the FMA neg_prod defect in task-326).

AC#1 (679a1b5) - the control word governs arithmetic. RC reached fist/fistp and nothing else; every add/sub/mul/div called F80 fixed at nearest-even/64-bit. F80 now takes a Ctl (the raw control word) and rounds ONCE against the precision-controlled width, in the direction RC selects. Two measurements: fldcw normalizes its operand (bit 6 forced set, bit 7 and 15:13 cleared - 0x0000 reads back 0x0040), which Unicorn does not do, so x87_fnstenv_env28_matches_unicorn gained a third recorded divergence where the measurement is the authority; and the engine's DEFAULT control word was zero, which under precision control means 24-bit single precision. That second one was caught by the REAL-PROGRAM LADDER, not by the 797-test suite - busybox awk's float printf - and CpuState::default is now written out field by field so adding a field is a compile error rather than a silent zero.

AC#3 + AC#2-state (ed802e8) - real stack emptiness and the environment round trip. CpuState gained fpu_empty, fpu_sw and fpu_env_tail, appended so no repr(C) offset moves. Four more hardware facts the SDM does not settle: the tag word does NOT round-trip verbatim (only the empty/non-empty pattern survives; a store re-derives the rest from the register contents); FXSAVE's abridged tag word is indexed by PHYSICAL register though the SDM writes it as "STj"; FXSAVE's ST slots ARE top-relative, and this wrote fpr[i] into slot i, so a save/restore pair rotated the whole stack whenever TOP was not 0 (pre-existing, invisible while the FTW was a constant 0xff); MXCSR_MASK is host-specific.

x87_tag_word_after_fninit_diverges_from_hardware asserted the divergent values on purpose so landing this would break it. It did.

METHOD NOTE. The FXSAVE witness exists because the first pass added that side of emptiness with nothing exercising it - breaking fxrstor's tag decode left every other x87 test green, which is how the two indexing bugs above were still there to find. And one round of negative controls silently targeted exec_fxstate instead of load_env28, because the same line appears in both: the script reported "0 failed" and I nearly took that as the tests being weak. Anchor a negative control on unique context.

WHAT IS NOT HERE. AC#2 also asked for restored exception flags to affect later execution. The flags now survive the round trip, which is what a fenv_t restore observes, but this FPU raises no FP exception at all - so a restored unmasked flag has nothing to act on. Setting the flags and delivering #MF is TASK-328; deferred.md points at it from the MXCSR entry.

VERIFIED: 799 unit tests green in debug and release, clippy, fmt, aarch64 cross-check, and the full 169-rung ladder (twice - it caught the default-control-word regression the first time).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
