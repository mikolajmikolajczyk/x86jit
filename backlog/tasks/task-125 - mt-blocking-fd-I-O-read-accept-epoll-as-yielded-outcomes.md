---
id: TASK-125
title: 'mt: blocking fd I/O (read/accept/epoll) as yielded outcomes'
status: Done
assignee: []
created_date: '2026-07-06 12:51'
updated_date: '2026-07-12 17:43'
labels:
  - 'crate:linux'
  - 'goal:feature'
milestone: go-caddy
dependencies: []
ordinal: 134000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Fable-5 scope. A guest thread blocking in read()/accept()/epoll must not hold the shim lock; needs the same yield-by-value treatment as futex (SyscallOutcome::Blocking*). Phase-3 territory (Go/caddy netpoller); P2.x must not touch it.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 mt test: blocking read on a pipe yields the vcpu and resumes with data (threaded driver observes no busy-spin)
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged bcef46d). Blocking read/readv/accept/accept4 in a threaded process now YIELD (SyscallOutcome::BlockingRead/BlockingAccept + ReadTarget::{Pipe,Host}) like epoll/futex, serviced by the driver after the guard drops (block_until in FUTEX_POLL chunks observing exited), instead of blocking under the shim lock. O_NONBLOCK guard keeps Go's netpoller on inline -EAGAIN (caught+fixed a go_http/go_net regression mid-impl). Pipe AC path fully safe. Reconciled onto main (was 715bbbd-based). 3 adversarial reviews. Perf: isolated go_http 36s (no regression), full suite 632/632. KNOWN LIMITATION filed as follow-up: post-wake libc::read/accept4 on a *blocking-mode* host fd runs under the re-acquired shim lock -> whole-process deadlock if two threads share one blocking fd and lose a readiness race (Medium; NOT a regression - blocking accept was already unsupported in threaded mode; Go-immune; not in any workload/test).
<!-- SECTION:NOTES:END -->
