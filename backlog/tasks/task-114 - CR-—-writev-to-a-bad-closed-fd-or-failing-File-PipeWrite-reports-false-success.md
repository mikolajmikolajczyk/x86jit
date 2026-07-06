---
id: TASK-114
title: CR — writev to a bad/closed fd or failing File/PipeWrite reports false success
status: To Do
assignee: []
created_date: '2026-07-06 11:10'
labels: []
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
