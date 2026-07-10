---
id: TASK-197
title: 'MODE-A: compat 32-bit flat — run Linux i386 user binaries'
status: To Do
assignee: []
created_date: '2026-07-10 10:31'
labels:
  - guest-modes
dependencies: []
references:
  - backlog/docs/design/spec.md
priority: medium
ordinal: 221000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Stage A of the pragmatic guest-mode plan: 32-bit protected/compat mode with flat segments (base 0 except FS/GS), enough to run Linux i386 user-space binaries 3-way (interp / JIT / unicorn diff).

Why: cheapest real second mode; validates all three spec §17 seams (CpuMode §17.3, BlockKey mode §17.4, effective_address §17.5) against a concrete consumer instead of a guessed abstraction. Groundwork every later mode (real16, full protected, V86) reuses.

Scope fence: NO segmentation beyond FS/GS bases, NO GDT/LDT/limits/rings, NO paging, NO runtime mode switching — Vm is constructed in one mode. Full protected mode (C1: descriptors/limits/exceptions, C2: paging/softmmu, V86) stays deliberately deferred until a machine-embedder consumer exists (spec §17.6). Legacy-only instructions (pusha, bound, into, aam/daa, les/lds, push seg) arrive trap-and-fix like AVX-512, not up front.

Subtasks carry the implementation; this parent is done when a real i386 Linux binary runs 3-way.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A real dynamically-or-statically-linked Linux i386 binary (e.g. Debian /bin/echo or a musl hello) runs to exit under interp and JIT with identical results
- [ ] #2 Unicorn 32-bit differential suite passes on the compat-mode lifter
- [ ] #3 Cache cannot confuse blocks across modes (mode is part of the block key, covered by a test)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
