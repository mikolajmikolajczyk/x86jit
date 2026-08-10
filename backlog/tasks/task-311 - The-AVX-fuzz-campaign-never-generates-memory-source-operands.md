---
id: TASK-311
title: The AVX fuzz campaign never generates memory-source operands
status: To Do
assignee: []
created_date: '2026-08-10 15:39'
labels: []
dependencies: []
priority: medium
ordinal: 347000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
fuzz.rs (~1358): the VEX pool's common shape emits YMM register operands, and the explicit entries follow it. So the campaign cannot falsify memory-source decoding, effective-address computation, load width, alignment or fault behaviour for any instruction it counts as covered.

That matters more than it would in isolation, because compat.rs over-reports the same axis (task-312): the coverage map says a Code is lifted based on its register form, and the fuzzer only exercises register forms. Memory-form gaps are invisible to both, which is how vextract*'s memory destination survived until a real PS4 binary hit it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Register and memory forms are selected independently for every applicable VEX entry
- [ ] #2 Alignment and page-boundary cases are generated
- [ ] #3 Coverage is reported per operand form, not per mnemonic
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
