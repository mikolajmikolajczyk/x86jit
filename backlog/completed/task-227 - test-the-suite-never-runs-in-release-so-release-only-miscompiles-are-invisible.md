---
id: TASK-227
title: >-
  test: the suite never runs in release, so release-only miscompiles are
  invisible
status: Done
assignee: []
created_date: '2026-07-29 10:58'
updated_date: '2026-08-11 11:07'
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
- [x] #1 A release-profile test leg exists and is documented in backlog/docs/commands.md
- [x] #2 The leg is wired into CI (both host arches) or has an explicit decision recorded if not
- [ ] #3 The known release-only failure (TASK-223) is caught by the new leg
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
A release leg exists: ci.yml runs the jit/differential/corpus/smc/mt binaries under --release, and commands.md documents both the CI form and the full local one. Deliberately a subset — the unicorn oracle and the fuzz campaigns dominate wall clock and are profile-independent; what this leg is for is codegen and softfloat, where the optimizer is the variable.

AC#3 CANNOT be met on this tree and that is the finding, not a gap: the known release-only failure (vmovs_vex_merge_takes_upper_from_vvvv, task-223) now PASSES under --release, because task-223's byte-wise merge workaround is in the tree and masks it. The whole suite was also run under --release while wiring this up: 763/763 passed, so no release-only miscompile is live today. The leg is worth having regardless — task-223 proved the class exists, and nothing else would have caught it.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
