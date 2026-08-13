---
id: TASK-330
title: Memory protection is not enforced on stores by either backend
status: To Do
assignee: []
created_date: '2026-08-13 12:01'
labels: []
dependencies: []
priority: medium
ordinal: 366000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
MEASURED: a guest store into a region mapped `Prot::RX` succeeds and changes the bytes, on BOTH the interpreter and the JIT.

    interp: exit=Hlt ro_page_now=0xdead
    jit:    exit=Hlt ro_page_now=0xdead

`Prot` (memory.rs:194) is recorded at `map` time and used for bounds/overlap checks, but no access path consults it. The engine models no permission faults at all — there is no `Exit` variant for one, only `UnmappedMemory`.

That may well be the right call for now; what is wrong is that it is written down NOWHERE. `deferred.md` has no entry, the README does not list it under known gaps, and `Prot` reads like an enforced protection to anyone using the embedder API. An embedder that maps its guest's `.text` read-only and expects a trap gets silent corruption instead.

Found while probing task-329. Decide deliberately: enforce it (needs a fault Exit and a story for how the embedder resumes — touches task-305), or keep it advisory and say so in the `Prot` doc comment, deferred.md and the README.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Either Prot is enforced on read/write/execute, or its doc comment states plainly that it is advisory and nothing checks it
- [ ] #2 deferred.md carries the decision with its reason, and the README's known gaps list it if it stays unenforced
- [ ] #3 A test pins whichever behaviour is chosen, on both backends
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
