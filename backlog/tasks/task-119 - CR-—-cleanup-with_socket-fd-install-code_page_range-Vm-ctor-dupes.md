---
id: TASK-119
title: 'CR — cleanup: with_socket / fd-install / code_page_range / Vm ctor dupes'
status: To Do
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:07'
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
socket-arm EBADF/host_errno skeleton x7 (with_socket helper), fd-install alloc+insert x5-6 (install(fd)), code_page_range(addr,len) span math x2 (mark_code/note_write), Vm::with_backend vs with_backend_host_ram struct-literal copy, iovec decode x2, scratch zero-fill x4.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
