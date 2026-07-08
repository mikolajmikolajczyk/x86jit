---
id: TASK-173
title: 'Linux shim: syscall dispatch boilerplate helpers (with_file/with_socket)'
status: To Do
assignee: []
created_date: '2026-07-08 20:29'
updated_date: '2026-07-08 20:40'
labels:
  - 'crate:linux'
  - 'goal:refactor'
  - seq-3
dependencies: []
ordinal: 197000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
LARGER/CAREFUL — the hottest embedder file (shim.rs handle() ~1368 LoC, 64 arms). Extract with_file(fd, op)/with_socket(fd, op)/fd_type() to collapse the 5-7 level nested fd-lookup+lock+downcast+errno matches (~30% of dispatch) and the 94x set_reg(Rax, ret) boilerplate. OPTIONAL stretch: unify handle() vs handle_mt() (80% overlap via delegate_mt) into one handle_syscall(mode: SyscallMode). Do AFTER the smaller wins land; needs the full corpus + mt/thread tests green to trust it. Lowers the syscall-porting tax.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 with_file/with_socket/fd_type helpers adopted; file/socket syscall arms shrink; full corpus + mt tests green
- [ ] #2 handle/handle_mt unification only if it stays clearly correct under the threaded tests (else leave split, documented)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
