---
id: TASK-114
title: CR — writev to a bad/closed fd or failing File/PipeWrite reports false success
status: Done
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:22'
labels:
  - 'crate:linux'
  - 'goal:fix'
milestone: code-review
dependencies: []
ordinal: 123000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
shim.rs SYS_WRITEV: the None/PipeRead/Stdin arm and the File/PipeWrite arms fall through to total += len instead of -EBADF / short. The socket arm was hardened; these siblings weren't. Pre-existing.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Fixed: SYS_WRITEV File/PipeRead/Stdin/Epoll/None arms no longer fall through to total+=len. File write failure -> EBADF (or short if earlier segments wrote); read-end/stdin/epoll/absent fd -> EBADF; read-only-passthrough File(None) swallows like SYS_WRITE. Mirrors the hardened socket arm. go_net green.
<!-- SECTION:NOTES:END -->
