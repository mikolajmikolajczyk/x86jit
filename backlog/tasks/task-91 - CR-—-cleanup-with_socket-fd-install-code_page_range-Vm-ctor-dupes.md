---
id: TASK-91
title: 'CR — cleanup: with_socket / fd-install / code_page_range / Vm ctor dupes'
status: To Do
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-08-10 21:45'
labels:
  - 'crate:linux'
  - 'crate:core'
  - 'goal:cleanup'
milestone: code-review
dependencies: []
ordinal: 128000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Duplication in the ENGINE worth folding, from a code-review pass. Scope narrowed 2026-08-10: the socket-arm EBADF/host_errno skeleton, the fd-install alloc+insert and the iovec decode all left with the syscall shim when the Linux userland moved to unemulinux, so they are that repository's business now (if still true there at all).

What remains here:
  - code_page_range(addr, len) span math, duplicated between mark_code and note_write (memory.rs)
  - Vm::with_backend vs with_backend_host_ram — struct-literal copy
  - scratch zero-fill, x4
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 pure refactor: no behavior change — existing suite green is the coverage (no new tests required)
<!-- AC:END -->
