---
id: TASK-115
title: CR — highest_mapped_below returns end for a region straddling the limit
status: To Do
assignee: []
created_date: '2026-07-06 11:10'
updated_date: '2026-07-07 10:02'
labels:
  - 'crate:core'
milestone: code-review
dependencies: []
ordinal: 124000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
memory.rs: filters on region start < limit but returns start+size, so a straddling region yields an address >= limit, violating the 'strictly below' contract.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
