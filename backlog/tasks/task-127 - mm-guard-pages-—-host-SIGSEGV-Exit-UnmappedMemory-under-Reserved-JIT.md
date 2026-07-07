---
id: TASK-127
title: 'mm: guard pages — host-SIGSEGV -> Exit::UnmappedMemory under Reserved JIT'
status: Done
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-07 12:21'
labels:
  - 'crate:core'
  - 'crate:linux'
  - 'goal:harden'
milestone: go-caddy
dependencies: []
ordinal: 136000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope; PRIORITY: right after P3. Under a Reserved span a Go nil-deref is in-span (page 0 < 1 TiB): JIT silently reads zero and continues where interp honestly traps (decision-3). Go semantically relies on nil-derefs faulting (nil-pointer panics). Fix: signal-safe host SIGSEGV handler recovers thread context, converts the hardware fault into a resumable Exit::UnmappedMemory. Closes decision-3 (flip its pinning test per the revisit clause). First thing you will want when caddy misbehaves under JIT only.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Guard pages complete GP-1..GP-4 (task-152 GP-5 host-back Flat path remains, separate). decision-3 superseded by decision-7. Commits: 8853c3b GP-1, 310a4bf GP-2, e172766 GP-3, GP-4 this commit. Closes the interp==JIT in-span-unmapped gap for host-backed spans.
<!-- SECTION:NOTES:END -->
