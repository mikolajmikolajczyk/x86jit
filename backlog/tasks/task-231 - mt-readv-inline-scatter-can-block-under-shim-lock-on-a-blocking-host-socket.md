---
id: TASK-231
title: 'mt: readv inline scatter can block under shim lock on a blocking host socket'
status: Done
assignee: []
created_date: '2026-07-12 17:44'
updated_date: '2026-07-12 18:05'
labels:
  - 'crate:linux'
  - 'goal:bug'
dependencies: []
ordinal: 260000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-125 review finding #2 (Low). readv_mt serves inline once the readability probe passes; if segment 1 is exactly filled (n==seg_len) and a segment 2 exists, the loop issues a second do_read -> one libc::read; on a blocking socket with no further data that call blocks inline under the shim lock (the exact thing 125 prevents for the first segment). Go breaks on segment-2 -EAGAIN so it only bites blocking-mode multi-segment readv. Same lock-held-blocking class as the read/accept deadlock; fold into that fix (nonblocking per-segment read + stop on EWOULDBLOCK).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 a multi-segment readv on a blocking host socket does not issue a blocking per-segment read under the shim lock; a short read is returned when later segments would block
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE (merged 7a22492, folded with 230). readv_mt probes fd_readable before each segment after the first; on would-block breaks and short-returns bytes-so-far (POSIX-legal short readv). No blocking libc::read under the lock on later segments. Files (host_io_fd None) skip the guard (never block). Test readv_mt_second_segment_would_block_short_reads: Rax=4, seg-2 guest memory untouched.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
