---
id: TASK-330
title: Memory protection is not enforced on stores by either backend
status: Done
assignee: []
created_date: '2026-08-13 12:01'
updated_date: '2026-08-13 21:36'
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
- [x] #1 Either Prot is enforced on read/write/execute, or its doc comment states plainly that it is advisory and nothing checks it
- [x] #2 deferred.md carries the decision with its reason, and the README's known gaps list it if it stays unenforced
- [x] #3 A test pins whichever behaviour is chosen, on both backends
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-08-13. Took the criterion's SECOND branch — `Prot` is documented as advisory and pinned by test — and the reason is technical, not convenience.

Enforcing it is not a check in the access path. The interpreter could consult `region_at` cheaply; the JIT's inlined accesses bound against `MemCtx.size` ALONE and carry no region map, by design (decision-3). So interpreter-only enforcement would manufacture exactly the backend divergence this project spends its effort removing — DEMONSTRATED, not asserted: injecting a prot check into `Memory::write` as a negative control fails the interpreter test and leaves the JIT one passing.

The mechanism that already solves this shape is decision-5's guard pages — the embedder's `HostRam::protect` hook `mprotect`s the host mapping so an in-span-unmapped access hardware-faults under the JIT at no hot-path cost. Reusing it for `Prot` means widening that hook from `accessible: bool` (PROT_READ|PROT_WRITE / PROT_NONE) to a permission, which is an embedder contract change ACROSS REPOSITORIES (unemulinux implements it), and it still would not cover a `Vec`-backed `Flat` span, which has no host pages to protect. Not a call to make unilaterally; recorded in deferred.md with the trigger (a JIT'd guest flipping W^X pages) and the note that `Exit::Exception { vector: 14 }` already exists, so delivering #PF needs no new `Exit`.

Recorded in three places, each of which a reader could plausibly consult first: the `Prot` doc comment (where the name promises otherwise), deferred.md, and the README's known gaps. `x86jit-tests/tests/prot_advisory.rs` pins the behaviour on both backends and says in its own header that enforcing it must fail this test first, so the records cannot be left behind claiming something that stopped being true.

Also worth recording: the first negative control did not apply at all — its anchor matched FOUR functions (read/write/atomic_rmw/atomic_cas share the shape) and the `count == 1` assert caught it. Without that assert it would have patched `read` and reported a clean run, which is the 'negative control silently hit the wrong function' trap this project has already paid for once.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
