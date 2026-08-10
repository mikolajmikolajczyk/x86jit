---
id: TASK-318
title: setup_stack hard-codes AT_RANDOM and the process credentials
status: To Do
assignee: []
created_date: '2026-08-10 15:40'
labels: []
dependencies: []
priority: medium
ordinal: 354000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x86jit-elf/src/lib.rs (~328): AT_RANDOM is always sixteen 0x5a bytes and UID/GID/EUID/EGID are always zero, with no way for the embedder to supply anything else.

Two separate consequences. Any guest stack canary or PRNG seeded from AT_RANDOM is fully predictable, which quietly removes the mitigation the guest thinks it has. And every guest is told it is root regardless of what the embedder intends, which is a policy decision the loader should not be making.

Determinism is genuinely wanted for the differential suite — a fixed AT_RANDOM is why runs are comparable — so the fix is to let the embedder supply the bytes and keep the constant as an explicit test-mode default, not to randomise unconditionally.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The embedder can supply AT_RANDOM bytes and process credentials
- [ ] #2 The deterministic values remain available explicitly, and the differential suite still compares
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
