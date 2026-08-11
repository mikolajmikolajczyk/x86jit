---
id: TASK-144
title: 'Machine Exit surface: IRQ injection + icount virtual-time exit (demand-driven)'
status: Done
assignee: []
created_date: '2026-07-10 10:34'
updated_date: '2026-08-11 11:01'
labels:
  - guest-modes
  - machine-exit
dependencies:
  - TASK-143
priority: low
ordinal: 229000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The two remaining core APIs a machine embedder needs beyond Exit::PortIo (TASK-142):

1. **IRQ injection** — embedder queues "deliver interrupt vector n"; the dispatcher delivers it at the next block boundary with the mode-correct frame (real: IVT flags/CS/IP push — protected/V86 frames arrive with their modes later). Needed for PIT/keyboard.
2. **Virtual-time exit** — run at most N guest instructions, then return control (`icount` is already tracked per block in the lifter — expose a budget in the run loop). Needed so an embedder can pace PIT ticks and throttle games that calibrate delay loops.

Also fold in SMC hardening review (fresh_code_pages, TASK-92) — DOS-era code self-modifies routinely.

DEMAND-DRIVEN like TASK-143 (same consumer, same §17.6 argument); depends on real16 for the first concrete interrupt frame.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Embedder can inject vector n; delivered at block boundary with correct real-mode frame (unicorn-diffed)
- [ ] #2 SMC: writes to translated code pages invalidate stale blocks under the run-loop (test with self-patching 16-bit blob)
- [ ] #3 run(budget) returns after <= budget guest instructions; test asserts interp and JIT report identical icount on a mixed branchy batch
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Closed into backlog/docs/deferred.md 2026-08-11 — the content is a decision not to build this, and deferred.md is where that belongs. Carrying it as an open task made the board claim work that nobody intends to start, and duplicated the document whose whole job is to say 'do not add this unprompted'.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
