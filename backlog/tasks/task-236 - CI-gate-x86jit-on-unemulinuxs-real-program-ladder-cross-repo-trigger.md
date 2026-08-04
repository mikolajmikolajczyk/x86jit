---
id: TASK-236
title: 'CI: gate x86jit on unemulinux''s real-program ladder (cross-repo trigger)'
status: To Do
assignee: []
created_date: '2026-08-04 14:41'
labels:
  - ci
  - infra
dependencies: []
ordinal: 332000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The Linux userland split moved the whole-program ladder — busybox, sqlite3, lua, djpeg, CPython, the Go servers, caddy — to unemulinux. The coverage did not go away, but the CI wiring to reach it does not exist yet, and until it does a lifter regression that only shows up in real software can land here with only ISA-level tests green.

Two facts make this worse than it sounds. This repository's workflow is `workflow_dispatch` only, so nothing runs automatically on a push at all. And the ladder is precisely the highest-signal validation the project has: the ISA corpus tells you what is wrong with the instructions that ARE lifted, never what a real program trips over.

Shape of the fix (pick one): a required job here that checks out unemulinux at a pinned revision, points its x86jit dependency at this commit, and runs its whole-program suite; or a repository_dispatch from here that unemulinux answers, reporting back. Either way the x86jit revision under test must be propagated rather than assumed, or the gate proves nothing.

A representative smoke subset (one static musl program, one dynamic glibc program, one Go binary) is an acceptable first step and much cheaper than the full ladder.

Raised by an adversarial review of the split; the finding is correct and the work is real.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A push or PR to this repository automatically exercises unemulinux's real-program ladder against that exact x86jit revision
- [ ] #2 The gate fails the build when the ladder fails, rather than reporting green because it never ran
- [ ] #3 status.md and README stop describing this as open work once it lands
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
