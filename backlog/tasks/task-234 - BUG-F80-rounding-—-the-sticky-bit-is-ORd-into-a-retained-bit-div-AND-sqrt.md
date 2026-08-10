---
id: TASK-234
title: 'BUG: F80 rounding — the sticky bit is OR''d into a retained bit (div AND sqrt)'
status: To Do
assignee: []
created_date: '2026-08-02 19:51'
updated_date: '2026-08-10 15:36'
labels:
  - bug
  - x87
  - f80
dependencies: []
ordinal: 330000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Found while validating TASK-233 (x87 integer-operand arithmetic) against Unicorn, but PRE-EXISTING and unrelated to that lift: task-233 does not touch f80.rs, and the already-lifted FLOAT forms (fdiv/fdivr with an m32fp/m64fp operand) reproduce the same wrong bits. So real guest code using long-double division is affected today.

Measured on this host — hardware via inline long double, engine via F80::div directly. Hardware and Unicorn agree with each other; we are the outlier:

    -9 / -3.5     hw 4000_a492492492492492    engine 4000_a492492492492493
     7 / 100.0    hw 3ffb_8f5c28f5c28f5c29    engine 3ffb_8f5c28f5c28f5c28
    100.0 / 7     hw 4002_e492492492492492    engine 4002_e492492492492493
    -3.5 / -9     hw 3ffd_c71c71c71c71c71c    engine 3ffd_c71c71c71c71c71c   (agrees)

Note the error goes BOTH ways (+1 ULP on two of them, -1 ULP on the third), so this is a rounding-decision defect, not a consistent bias.

Suspected cause, f80.rs around lines 333-344 in F80::div:

    let num = (a.sig as u128) << 64;
    let q = num / (b.sig as u128);
    let rem = num % (b.sig as u128);
    let m = if rem != 0 { q | 1 } else { q };
    normalize_round(sign, a.exp - b.exp + 63, m)

The comment says q is a 65-bit quotient with 64 fraction bits. Folding the remainder in as `q | 1` assumes bit 0 of q is always a bit that normalize_round will discard. That holds when the quotient really is 65 bits and gets shifted right by one, but not when it is 64 bits and no shift happens — there the sticky bit lands directly in the result's LSB and either corrupts an otherwise-exact low bit or pushes a round-to-nearest-even decision the wrong way. Whichever the exact mechanism, it needs deriving rather than patching by trial: get the guard/round/sticky bits right relative to how many bits normalize_round actually drops.

Check F80::mul for the same pattern before assuming div is the only victim — it has a structurally similar normalize_round call with a shifted significand product.

Note that TASK-233 deliberately did NOT fix this (bug fix = bug fix). Its differential test excludes the two divergent cases with the measured bytes written into the doc comment, and covers them instead by asserting the integer form is bit-identical to the float form. Once this is fixed, those exclusions should be removed and the cases folded back into the Unicorn comparison.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 F80::div matches hardware bit-exactly on the four cases above and on a broader inexact-quotient sweep
- [ ] #2 The fix is derived from the guard/round/sticky positions relative to what normalize_round discards, not tuned until the tests pass
- [ ] #3 F80::mul is checked for the same sticky-bit pattern, and either fixed too or shown to be correct
- [ ] #4 The TASK-233 differential exclusions in x87_int_arith_equals_float_arith / x87_integer_operand_arith_matches_unicorn are removed and those cases compare against Unicorn again
- [ ] #5 sqrt is bit-exact against hardware on a directed corpus incl. sqrt(2), sqrt(3), sqrt(7), sqrt(10), sqrt(0.5)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
WIDENED 2026-08-10 after an adversarial review + measurement. This is not a division bug; it is the rounding path. F80::sqrt does the same thing at f80.rs:362 (`let m = if exact { root } else { root | 1 }`): isqrt128 already returns a 64-bit root, so normalize_round performs no further shift and the OR changes the value instead of acting as a sticky bit. Measured against this host's `fsqrt`, 5 of 6 probed inputs are wrong by 1 ULP: sqrt(2) ours 0xb504f333f9de6485 vs hw ...84; sqrt(3) ...9d vs ...9e; sqrt(7) ...bd vs ...bc; sqrt(10) ...91 vs ...90; sqrt(0.5) ...85 vs ...84. sqrt(5) agrees by luck. Because root|1 forces an odd significand, an inexact result is correct only when true rounding also wanted the low bit set. Fix the rounding contract once (compare the remainder against the exact half-ULP boundary, ties-to-even) rather than patching div and sqrt separately, and check rem/transcendentals for the same shape.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
