---
id: TASK-184
title: 'CI/pre-commit guard: reject guest-specific references in engine source'
status: Done
assignee: []
created_date: '2026-07-15 12:16'
updated_date: '2026-08-11 11:13'
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
- [x] #1 A grep-gate (pre-commit hook and/or CI step) scans tracked *.rs source (excluding backlog/ docs and this task) and FAILS if any guest/downstream term appears: case-insensitive match on a curated denylist (e.g. celeste, mono, doom, ps4, playstation, orbis, unemups4, sgen, monogame, fmod, homebrew) as whole-words where sensible
- [x] #2 The denylist lives in one obvious place with a comment explaining the guest-agnostic rule and how to extend it
- [x] #3 The gate is wired so it runs in the same place existing lint runs (mirror how clippy/fmt are gated) and documents the exact command to run locally
- [x] #4 Running the gate on current HEAD passes (repo is clean today); a deliberately-inserted 'Celeste' comment makes it fail (proves it works)
- [ ] #5 Motivating-context for a lift is expressed generically (semantics + encoding + local x86jit task id only) — the guard does NOT block the word 'guest' itself or generic ISA terms
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed as scripts/guest-agnostic-guard.sh, wired into pre-commit (per-commit — it is a grep) and into CI beside clippy. Documented in the script's own header.

THE TASK'S PREMISE WAS FALSE and that is the result worth keeping. It said 'a scan today is clean; nothing prevents regression'. The first case-insensitive run found 63 references across 21 files, in x86jit-core and x86jit-cranelift as well as the tests: Celeste, Mono/MonoGame, Little Nightmares, Doom, Orbis, PS4. All rewritten to keep the technical fact and drop the name — 'the exact reported wall bytes c4 e3 79 21 d1 10' says everything the old comment said that a reader of this repository needs.

Two defects in the guard itself, both found by running it rather than by reading it:
  - The denylist was wrong three ways: it listed `unemulinux` (the sibling embedder, which boundary.rs exists to name), `fmod` (C's remainder, cited legitimately in f80.rs), and `pkg` (matches pkg-config). Fixed, with the reasoning recorded in the script so the next person does not re-add them.
  - The first version had no -i despite a comment claiming it did, so a planted 'Celeste' passed. That is the exact class this whole session has been finding: a gate that cannot fail. Caught by planting one; the negative control is now the proof it works.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
