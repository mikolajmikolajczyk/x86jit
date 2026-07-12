---
id: TASK-230
title: >-
  mt: blocking read/accept post-wake syscall runs under shim lock -> deadlock on
  lost readiness race
status: Done
assignee: []
created_date: '2026-07-12 17:44'
updated_date: '2026-07-12 18:05'
labels:
  - 'crate:linux'
  - 'goal:bug'
dependencies: []
ordinal: 259000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-125 review finding #1 (Medium). read_ready (Host arm, raw libc::read) and do_accept (raw libc::accept4) run under the RE-ACQUIRED shim lock after block_until reports readiness. Readiness is probed level-triggered OUTSIDE the lock. If two guest threads share one BLOCKING-mode host fd (shared fd_table in a CLONE_VM process, or dup), both wake on one ready event; the first drains it, the second's blocking read/accept4 then blocks on an empty fd WHILE HOLDING the global shim lock -> whole-process deadlock (every sibling's syscall stalls). NOT a regression (blocking accept was unsupported/blocking pre-125); Go-immune (nonblocking + netpoller); not hit by any current workload/test. Fix: under the re-acquired lock, poll(fd,POLLIN,0) first; if not ready (lost race) re-park via the driver loop (read_ready/accept_ready return an Option/sentinel = would-block; the BlockingRead/BlockingAccept arms loop block_until again) instead of the raw blocking syscall. Since completions serialize on the shim lock, poll-then-syscall under the lock is atomic w.r.t. siblings.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 two threaded guests sharing one blocking-mode host socket both blocked in read (or accept) with a single ready event do NOT deadlock; the loser re-parks and resumes on the next ready event
- [x] #2 a regression test drives the two-reader-one-ready-event race and completes without hang
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged 7a22492). read_ready(Host)/accept_ready now poll(fd,POLLIN,0) via fd_readable BEFORE the syscall, under the re-acquired shim lock; not readable -> return None (would-block). Return type u64->Option<u64>. Driver BlockingRead/BlockingAccept arms loop: block_until -> complete -> Some(r) set Rax+break / None re-park. Since all consuming host syscalls serialize on the shim lock, poll-then-syscall under the lock is atomic w.r.t. siblings; readiness can only be ADDED by non-lock paths (writes/POLLHUP), never consumed. Inline + single-threaded accept untouched. Deadlock confirmed empirically (neutralized guard -> 5s timeout; restored -> 5ms). Adversarial review: no bugs. 636/636.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
