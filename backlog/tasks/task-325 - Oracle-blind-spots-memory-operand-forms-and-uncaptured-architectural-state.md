---
id: TASK-325
title: 'Oracle blind spots: memory operand forms and uncaptured architectural state'
status: In Progress
assignee: []
created_date: '2026-08-11 11:03'
updated_date: '2026-08-11 14:46'
labels: []
dependencies: []
priority: medium
ordinal: 361000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Three findings that share one consequence: a class of defect this project cannot currently detect, so the coverage it reports is an upper bound rather than a measurement.

**The compat probe instantiates register forms only** (compat.rs ~193). Every *_or_mem / *_rm operand kind is built as a register, and iced puts both alternatives under one Code — so lifting the register encoding marks the whole Code lifted. Memory-only shapes fall into the unencodable bucket and vanish. A generator-emitted caveat now says so in the published map (task-312); the probe is still wrong.

**The AVX fuzz campaign generates no memory operands** (fuzz.rs ~1358). The VEX pool's common shape emits YMM registers and the explicit entries follow it, so the campaign cannot falsify memory-source decoding, effective-address computation, load width, alignment or fault behaviour for anything it counts as covered.

Those two compound: the map says a Code is covered on the strength of its register form, and the fuzzer only exercises register forms. Memory-form gaps are invisible to both, which is how vextract*'s memory destination survived until a real PS4 binary trapped on it.

**The native oracle captures a subset of architectural state** (native.rs ~541). x87 state is defaulted, the snapshot model has no MXCSR, and vector capture stops at register 15. A snippet can set the wrong rounding mode, raise the wrong FP exception flags, corrupt the x87 status or tag word, or write ZMM16-31 wrongly and still match every compared field. The README now states this narrowing (task-313).

Merged from task-320, 311, 321.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Register and memory forms are probed independently; a Code is fully lifted only when every form lifts, otherwise partial naming the failing form
- [ ] #2 The fuzzer selects register and memory forms independently, varying alignment and page boundaries, and reports coverage per operand form
- [ ] #3 The regenerated coverage map's newly-revealed gaps are triaged
- [ ] #4 MXCSR, full x87 state and ZMM16-31 are captured and compared
- [ ] #5 The generator caveat and the README narrowing added by task-312/313 are removed once each stops being true
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
