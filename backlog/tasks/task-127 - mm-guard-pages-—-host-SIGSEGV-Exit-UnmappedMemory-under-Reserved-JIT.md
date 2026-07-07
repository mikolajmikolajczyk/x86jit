---
id: TASK-127
title: 'mm: guard pages — host-SIGSEGV -> Exit::UnmappedMemory under Reserved JIT'
status: To Do
assignee: []
created_date: '2026-07-06 13:40'
updated_date: '2026-07-07 11:27'
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
Session pause 2026-07-07. Done: GP-1 (guard-page protect plumbing, 8853c3b) + GP-2 (SIGSEGV->resumable Exit::UnmappedMemory, 310a4bf) — the hard unsafe signal-handling feature LANDS and is wired (thread.rs/proc.rs guarded_run; x86jit-run Go path reserve_guarded). Go nil-derefs under JIT now fault. All Go + driver tests green under guards; honesty test passes. Next: GP-3 (task-150, precise faulting RIP via srcloc side-table + CodeMap in core — RIP currently stale at async fault), then GP-4 (task-151, decision-7 supersede decision-3 + close 127), GP-5 (task-152, host-back Flat path). Design: doc-30. Blocker: none. glibc host assumption in sigsegv.rs.
<!-- SECTION:NOTES:END -->
