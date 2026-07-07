---
id: TASK-150
title: GP-3 — precise faulting RIP (srcloc side table)
status: To Do
assignee: []
created_date: '2026-07-07 11:02'
labels:
  - go-caddy
  - 'crate:core'
  - 'goal:harden'
dependencies: []
ordinal: 159000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
doc-30 GP-3. set_srcloc(guest_rip u32) at InsnStart; capture srclocs+code size at compile; CodeMap in core (append-only, AS-safe read); handler host-PC->guest-RIP->cpu.rip. Tests: RIP parity interp==JIT; region-mode RIP exact; single-block GPR parity.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
