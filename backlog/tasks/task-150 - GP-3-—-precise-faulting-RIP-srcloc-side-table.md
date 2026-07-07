---
id: TASK-150
title: GP-3 — precise faulting RIP (srcloc side table)
status: Done
assignee: []
created_date: '2026-07-07 11:02'
updated_date: '2026-07-07 12:12'
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

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
GP-3 implemented (uncommitted). CodeMap in x86jit-core/src/codemap.rs (append-only chunked, AS-safe read; static CODE_MAP + register/lookup). codegen InsnStart sets SourceLoc(guest_rip u32). compile_with captures total_size + get_srclocs_sorted -> codemap::register(entry,len,table). sigsegv.rs: fault_pc D4 seam + GuardSlot.fault_pc + guarded_run recovers precise RIP via codemap::lookup. 3 new guard_pages tests (single-block RIP parity/exact, region-mode mid-superblock RIP, single-block GPR fault-before-commit) + 2 codemap unit tests. Full suite 250 pass, clippy+fmt clean. Next: GP-4 (task-151).
<!-- SECTION:NOTES:END -->
