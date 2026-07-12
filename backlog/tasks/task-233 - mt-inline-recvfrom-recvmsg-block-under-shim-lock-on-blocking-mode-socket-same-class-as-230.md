---
id: TASK-233
title: >-
  mt: inline recvfrom/recvmsg block under shim lock on blocking-mode socket
  (same class as 230)
status: Done
assignee: []
created_date: '2026-07-12 18:05'
updated_date: '2026-07-12 18:58'
labels:
  - 'crate:linux'
  - 'goal:bug'
dependencies: []
ordinal: 262000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-230 review out-of-scope observation. handle_mt's inline recvfrom (shim.rs ~2580) / recvmsg (~2701) still issue a potentially-blocking host syscall under the shim lock for a blocking-mode socket with no data — the same whole-process deadlock class 230 fixed for read/accept. NOT a regression (pre-existing); Go/netpoller-immune (O_NONBLOCK sockets). If blocking-mode recv* is in scope, apply the same poll-under-lock + yield/re-park treatment (a BlockingRecv outcome, or route recvfrom/recvmsg through the read_mt would-block path).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 a threaded blocking-mode recvfrom/recvmsg on an empty socket yields + re-parks instead of blocking under the shim lock; a two-reader race does not deadlock
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged 6c118bd). recvfrom/recvmsg in handle_mt now yield SyscallOutcome::BlockingRecv(Box) + driver re-parks (mirror of task-230), instead of blocking libc::recvfrom/recvmsg under the shim lock. Inline writeback factored into do_recvfrom/do_recvmsg (moved verbatim -> byte-identical, review-confirmed). Yield only for a blocking-mode socket with no data; nonblocking OR MSG_DONTWAIT OR ready -> inline -EAGAIN/data. REVIEW-CAUGHT (High, fixed in-branch): MSG_DONTWAIT (per-call nonblock flag, unlike read/accept) was ignored in the park decision -> a blocking-mode socket + MSG_DONTWAIT parked forever instead of -EAGAIN; now checks flags & MSG_DONTWAIT. Deadlock confirmed empirically (neutralized guard -> 5s timeout). 641 passed, clippy+fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
