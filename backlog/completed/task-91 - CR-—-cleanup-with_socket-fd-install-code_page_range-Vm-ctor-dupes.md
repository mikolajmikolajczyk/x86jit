---
id: TASK-91
title: 'CR — cleanup: with_socket / fd-install / code_page_range / Vm ctor dupes'
status: Done
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-08-11 11:30'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Delivered the one item that was real: code_page_range(addr, len) is extracted in memory.rs and used by both mark_code and note_write. It is a dedup, not an optimisation — the three things that are easy to get subtly wrong in one of two copies (len.max(1) so a zero-length access still names its page, saturating_add so a top-of-address-space access cannot wrap to page 0, and an inclusive range so a boundary-spanning access marks both pages) are now stated once, with the reason.

The other two items are stale:
  - 'Vm::with_backend vs with_backend_host_ram struct-literal copy' is already fixed. Both delegate to from_mem; the duplication went away since the task was written.
  - 'scratch zero-fill x4' is the same item as task-92's, and it was measured there — see that task.
<!-- SECTION:NOTES:END -->
