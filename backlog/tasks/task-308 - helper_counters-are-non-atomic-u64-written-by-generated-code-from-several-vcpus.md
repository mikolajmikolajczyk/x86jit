---
id: TASK-308
title: >-
  helper_counters are non-atomic u64 written by generated code from several
  vcpus
status: To Do
assignee: []
created_date: '2026-08-10 15:38'
labels: []
dependencies: []
priority: medium
ordinal: 344000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
cranelift/src/lib.rs:1974 stores them as Box<[u64]>; every compiled helper call emits a plain load/add/store, and reporting reads the same words concurrently.

Known and accepted as 'lost updates make the diagnostic approximate'. The sharper objection is that concurrent unsynchronised access from generated code is a data race in the host program, so the diagnostic is UB rather than merely imprecise. A counter is not worth that.

AtomicU64 with a relaxed increment, or per-vcpu counters aggregated at read.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Counters are race-free; reporting uses atomic loads
- [ ] #2 The relaxed increment's cost on the helper path is measured, not assumed
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
