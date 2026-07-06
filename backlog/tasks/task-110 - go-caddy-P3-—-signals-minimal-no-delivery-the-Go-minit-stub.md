---
id: TASK-110
title: go-caddy P3 — signals-minimal (no delivery; the Go minit stub)
status: To Do
assignee: []
created_date: '2026-07-06 11:09'
labels: []
milestone: go-caddy
dependencies: []
ordinal: 119000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
sigaltstack / rt_sigprocmask / rt_sigaction stubs Go's minit needs (a 2-line stub is a P2->DoD-2 dependency; sigaltstack -ENOSYS is fatal for Go). No real delivery. Also the guard-page path for the JIT/interp unmapped-in-span decision would live here.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 static Go hello world prints hello three ways (native/interp/JIT)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
