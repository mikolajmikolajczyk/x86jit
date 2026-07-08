---
id: TASK-171
title: 'Embedder API ergonomics: RunOptions + VmConfig constructors'
status: To Do
assignee: []
created_date: '2026-07-08 20:29'
updated_date: '2026-07-08 20:40'
labels:
  - 'crate:run'
  - 'crate:core'
  - 'goal:refactor'
  - 'goal:api'
  - seq-1
dependencies: []
ordinal: 194000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The 'nicer to use later' win. (1) Collapse the 5-layer run_config_argv{,_stdin,_features} delegation chain into one entry + a RunOptions{stdin, features, ..} struct (defaults), so future knobs (tier-up, mem limits) don't grow a new wrapper each time. (2) VmConfig::flat(size)/::reserved(span) constructors — ~65 identical 'VmConfig{memory_model, consistency:Fast}' literals across run/linux/elf/bench/tests collapse to one line. Zero runtime cost. Embedder-facing, so worth doing before more embedders appear. Survey 2026-07-08.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Single run_config_argv entry + RunOptions struct; old wrappers gone or thin shims
- [ ] #2 VmConfig::flat/::reserved constructors; the ~65 literal sites use them
- [ ] #3 No behavior change; suite + run-crate tests green
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
