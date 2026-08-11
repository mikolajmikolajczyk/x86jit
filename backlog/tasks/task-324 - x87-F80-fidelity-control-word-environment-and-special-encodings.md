---
id: TASK-324
title: 'x87/F80 fidelity: control word, environment, and special encodings'
status: To Do
assignee: []
created_date: '2026-08-11 11:02'
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
- [ ] #1 RC and PC reach every arithmetic operation and change its result; tested under all four rounding modes and all three precisions against hardware
- [ ] #2 An arbitrary 28-byte environment image survives fldenv then fnstenv unchanged, and restored exception flags affect later execution
- [ ] #3 The tag word reports 11 for empty; x87_tag_word_after_fninit_diverges_from_hardware is updated to assert the correct values
- [ ] #4 Unsupported, signaling-NaN and quiet-NaN are distinct classes with x87 invalid-operation and NaN-selection rules
- [ ] #5 Raw 10-byte witnesses cover pseudo-denormals, unnormals, both NaN kinds, signs and payloads
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
