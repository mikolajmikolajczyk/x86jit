---
id: TASK-236
title: 'CI: gate x86jit on unemulinux''s real-program ladder (cross-repo trigger)'
status: To Do
assignee: []
created_date: '2026-08-04 14:41'
updated_date: '2026-08-09 18:57'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Local half landed 2026-08-09: scripts/ladder.sh drives unemulinux's ladder from here. Modes: smoke (default, ~30 s — one static musl, one dynamic glibc, one Go binary, one busybox applet), --full (~10 min), --rev <ref> (detached x86jit worktree; the userland is always the working tree next door, so a harness change and the engine change it exercises test together). Wired as a pre-push hook via --if-present, which prints a skip when unemulinux is absent instead of blocking a push; the plain form exits 2, never 0, so 'did not run' cannot read as 'passed'. Verified by negative control: an injected off-by-one in interp alu_add turned 4 of the 6 smoke tests red and the script exited 100 with no 'passed' line. Cargo's paths override was rejected as the mechanism for --rev (cargo itself warns it 'is known to produce buggy behavior' and will become a hard error). STILL OPEN: the CI half. unemulinux's .github/workflows/ci.yml now accepts a repository_dispatch of type 'ladder' carrying x86jit_ref, but nothing here sends it — AC#1 and AC#2 are not met.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
