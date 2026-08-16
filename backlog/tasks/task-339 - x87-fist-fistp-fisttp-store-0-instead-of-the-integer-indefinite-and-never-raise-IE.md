---
id: TASK-339
title: >-
  x87: fist/fistp/fisttp store 0 instead of the integer indefinite and never
  raise IE
status: To Do
assignee: []
created_date: '2026-08-15 13:25'
labels:
  - m6-x87
dependencies: []
priority: high
ordinal: 375000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`operand!(cpu,0).to_i64_rc(..) as i16` truncates i64::MIN to 0x0000; `as i32` likewise. Only the i64 forms are accidentally right, and none of the six sites raises IE.

    engine:   word(1e30)=0x0000  word(40000)=0x9c40  dword(1e30)=0x00000000  sw=0x0000 (IE=0)
    hardware: word(1e30)=0x8000  word(40000)=0x8000  dword(1e30)=0x80000000  sw=0x0001 (IE=1)

SDM Vol 1 §8.2.1: 'If the x87 FPU detects an invalid operation when storing an integer value in memory with an FIST/FISTP instruction and the invalid-operation exception is masked, the x87 FPU stores the integer indefinite encoding in the destination operand' — 100..00B at the destination width. SDM Vol 1 Table 8-10 lists the same for out-of-range, SNaN, QNaN and infinity.

The 40000.0 -> 0x9c40 case is the dangerous one: no NaN or infinity, just a value that fits i64 and not i16, and the wrong answer is a plausible number rather than a sentinel.

Guest sees a silently wrong result; nothing traps even with IM unmasked, because IE is never raised.

Sites: x87.rs:883,890,906,913 store the wrong value (FistpI16, FistpI32, FisttpI16, FisttpI32); all 6 to_i64_rc call sites fail to raise IE. f80.rs:487-489 documents the invariant the callers break — 'the caller masks to the destination width' — but masking i64::MIN to 16 bits is 0, not 0x8000.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every fist/fistp/fisttp width stores the width's integer indefinite on an out-of-range or NaN source, and raises IE
- [ ] #2 Unmasked IM abandons the store instead of writing a value
- [ ] #3 A host-witnessed test covers the in-range-for-i64-but-not-i16 case, not just NaN and infinity
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
