---
id: TASK-313
title: 'The native oracle does not capture MXCSR, x87 state, or ZMM16-31'
status: Done
assignee: []
created_date: '2026-08-10 15:39'
updated_date: '2026-08-10 19:29'
labels: []
dependencies: []
priority: medium
ordinal: 349000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
native.rs (~541) substitutes defaults for x87 state; the snapshot model has no MXCSR and captures vector registers 0-15 only.

So a snippet can set the wrong rounding mode, raise the wrong FP exception flags, corrupt the x87 status or tag word, or write ZMM16-31 incorrectly, and still match on every field the comparison looks at. The README's 'cross-checked three ways' is true for the state that is captured and silent about the rest.

Either capture it from the signal frame, or narrow the claim in the README and testing docs. Narrowing is legitimate; leaving the claim broad while the capture is narrow is not.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 MXCSR, full x87 state and ZMM16-31 are captured and compared, or the documented claim is narrowed to match what is
- [ ] #2 A test that deliberately dirties each uncaptured field fails once it is captured
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Resolved as a documentation fix. README's three-way claim now names what the native leg captures (GPRs, flags, XMM/YMM/ZMM0-15, opmasks) and what it does not (MXCSR, x87 status/tag, ZMM16-31). Extending the capture is the better fix and is still open as task-321.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
