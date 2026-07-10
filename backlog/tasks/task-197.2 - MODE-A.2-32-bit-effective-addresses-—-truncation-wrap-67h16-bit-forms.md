---
id: TASK-197.2
title: 'MODE-A.2: 32-bit effective addresses — truncation/wrap + 67h(16-bit) forms'
status: In Progress
assignee: []
created_date: '2026-07-10 10:32'
updated_date: '2026-07-10 11:35'
labels:
  - guest-modes
dependencies:
  - TASK-197.1
parent_task_id: TASK-197
ordinal: 223000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
In Compat32 `effective_address` computes mod 2^32 (base+index*scale+disp wraps at 4 GiB, result zero-extended for the flat Memory lookup); the 67h prefix selects 16-bit addressing (mod 2^16, classic ModRM forms — iced decodes them, the lowering must not assume SIB). Stays a change inside the single helper per seam §17.5. RIP-relative does not exist in 32-bit mode — guard that path by mode.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Address arithmetic wraps at 32 bits in Compat32 (unicorn-diffed, incl. negative displacement wrap cases)
- [ ] #2 67h-prefixed 16-bit addressing forms compute correctly (unicorn-diffed)
- [ ] #3 lea honours address-size truncation without adding segment bases (unicorn-diffed, incl. a seg-prefixed lea case)
<!-- AC:END -->





## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
