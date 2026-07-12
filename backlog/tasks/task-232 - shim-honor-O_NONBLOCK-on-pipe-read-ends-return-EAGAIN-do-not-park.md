---
id: TASK-232
title: 'shim: honor O_NONBLOCK on pipe read ends (return -EAGAIN, do not park)'
status: To Do
assignee: []
created_date: '2026-07-12 17:44'
labels:
  - 'crate:linux'
  - 'goal:fidelity'
dependencies: []
ordinal: 261000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-125 review finding #3 (Low, pre-existing). read_would_block (pipe arm) yields BlockingRead for an empty pipe with a live writer regardless of O_NONBLOCK, and F_SETFL only forwards O_NONBLOCK to host_io_fd fds, never to PipeRead. A guest that sets O_NONBLOCK on a pipe read end (self-pipe / event-loop idiom) and expects immediate -EAGAIN on empty is instead parked until data/writer-close. Pre-existing: the shim never honored O_NONBLOCK on pipes. Track pipe O_NONBLOCK state and return -EAGAIN inline for a nonblocking empty pipe read.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 a guest read of an empty O_NONBLOCK pipe with a live writer returns -EAGAIN inline (Continue), not a BlockingRead yield
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
