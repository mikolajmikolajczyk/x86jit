---
id: TASK-318
title: setup_stack hard-codes AT_RANDOM and the process credentials
status: Done
assignee: []
created_date: '2026-08-10 15:40'
updated_date: '2026-08-11 12:14'
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
- [x] #1 The embedder can supply AT_RANDOM bytes and process credentials
- [x] #2 The deterministic values remain available explicitly, and the differential suite still compares
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
ProcIdentity carries the 16 AT_RANDOM bytes and uid/euid/gid/egid; setup_stack_as and setup_stack_dyn_as take one. setup_stack and setup_stack_dyn keep their signatures and pass ProcIdentity::deterministic(), so no caller changed and the differential suite still compares two runs byte-for-byte — which it can only do if the initial stack is identical, and which is why the fixed values had to stay reachable rather than be replaced by real entropy.

Two problems were wearing one hat here and both are now the embedder's call: a guest seeds its stack canary and often its PRNG from AT_RANDOM, so a constant silently removes a mitigation the guest believes it has; and telling every process it is root is a policy decision a loader should not make.

Test the_embedder_chooses_entropy_and_identity asserts both halves — the supplied bytes reach the address AT_RANDOM points at, the ids reach their auxv entries — and pins that the default is still exactly 0x5a x16 with uid 0.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
