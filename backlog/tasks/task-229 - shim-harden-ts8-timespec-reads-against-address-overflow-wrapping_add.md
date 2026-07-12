---
id: TASK-229
title: 'shim: harden ts+8 timespec reads against address overflow (wrapping_add)'
status: Done
assignee: []
created_date: '2026-07-12 17:12'
updated_date: '2026-07-12 19:12'
labels:
  - 'crate:linux'
  - 'goal:robustness'
dependencies: []
ordinal: 258000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-121 review, pre-existing latent. ~10 sites do read_u64(vm, ts + 8) / ts + 8 with a plain add on a guest-controlled pointer (nanosleep, clock_nanosleep, futex timespec, etc.); with ts ~ u64::MAX the add panics in debug/test builds (overflow-checks) = a self-inflicted DoS. abs_deadline_to_rel and walk_robust_list already use saturating/wrapping math. Sweep the timespec/timeval read sites to wrapping_add for consistency.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 guest timespec/timeval reads near u64::MAX do not panic in debug builds; a test drives a syscall with a near-max pointer and gets a clean errno/degradation
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged 738696a). wrapping_add on guest-controlled pointer arithmetic feeding read/write (timespec ts+8, msghdr msgp+16/24/32/40/48, poll, iovec, epoll writeback) — avoids debug overflow-panic on a near-u64::MAX guest pointer; behavior-identical for valid pointers (all bounds-checked via vm read/write). do_recvmsg hardened during 233 reconcile. Review: no wrong-region read introduced. Overflow test drives FUTEX ts=u64::MAX-3.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
