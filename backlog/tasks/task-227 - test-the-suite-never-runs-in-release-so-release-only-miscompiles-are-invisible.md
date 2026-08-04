---
id: TASK-227
title: >-
  test: the suite never runs in release, so release-only miscompiles are
  invisible
status: To Do
assignee: []
created_date: '2026-07-29 10:58'
labels:
  - bug
  - testing
  - ci
dependencies: []
ordinal: 323000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
cargo nextest run builds the dev profile. Every one of the 696 tests therefore exercises x86jit-core at opt-level 0. A defect that only appears under optimization cannot be caught by any of them.

This is not hypothetical — it is how TASK-223 survived. The regression test vmovs_vex_merge_takes_upper_from_vvvv (x86jit-tests/tests/jit.rs) PASSES under cargo test and FAILS under cargo test --release, on the same tree:

  cargo test         -p x86jit-tests --test jit vmovs_vex_merge   ok
  cargo test --release -p x86jit-tests --test jit vmovs_vex_merge   FAILED

And the embedder runs release, so the engine users actually execute is the one nothing tests.

Wanted: a release leg. It does not have to be the whole suite on every run — the semantic core (jit.rs, differential*, interpreter.rs, fuzz smoke, corpus) in release is the part that matters, since that is where a miscompiled interpreter shows up. Decide whether it is a CI job, a nextest profile, or both, and write it into backlog/docs/commands.md so the dev loop is not silently debug-only.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A release-profile test leg exists and is documented in backlog/docs/commands.md
- [ ] #2 The leg is wired into CI (both host arches) or has an explicit decision recorded if not
- [ ] #3 The known release-only failure (TASK-223) is caught by the new leg
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
