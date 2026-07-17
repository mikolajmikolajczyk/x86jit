---
id: TASK-250
title: 'CI/pre-commit guard: reject guest-specific references in engine source'
status: To Do
assignee: []
created_date: '2026-07-15 12:16'
labels:
  - hygiene
  - ci
  - guest-agnostic
dependencies: []
ordinal: 280000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
x86jit is a guest-AGNOSTIC x86-64 emulation engine. Downstream consumers (an emulator, its games, its runtimes) must never be named in engine source — comments or identifiers. Subagents driving lifts on behalf of a downstream project have leaked such context before (game names, guest runtime names, downstream task ids) and it needs a mechanical gate, since a scan today is clean but nothing prevents regression.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A grep-gate (pre-commit hook and/or CI step) scans tracked *.rs source (excluding backlog/ docs and this task) and FAILS if any guest/downstream term appears: case-insensitive match on a curated denylist (e.g. celeste, mono, doom, ps4, playstation, orbis, unemups4, sgen, monogame, fmod, homebrew) as whole-words where sensible
- [ ] #2 The denylist lives in one obvious place with a comment explaining the guest-agnostic rule and how to extend it
- [ ] #3 The gate is wired so it runs in the same place existing lint runs (mirror how clippy/fmt are gated) and documents the exact command to run locally
- [ ] #4 Running the gate on current HEAD passes (repo is clean today); a deliberately-inserted 'Celeste' comment makes it fail (proves it works)
- [ ] #5 Motivating-context for a lift is expressed generically (semantics + encoding + local x86jit task id only) — the guard does NOT block the word 'guest' itself or generic ISA terms
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
