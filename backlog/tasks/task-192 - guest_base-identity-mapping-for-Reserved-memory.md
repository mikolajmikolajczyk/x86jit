---
id: TASK-192
title: guest_base identity mapping for Reserved memory
status: Done
assignee: []
created_date: '2026-07-09 15:19'
updated_date: '2026-07-09 16:27'
labels: []
dependencies: []
ordinal: 216000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Add guest_base:u64 (default 0) to HostRam/Memory so an embedder achieves host==guest identity mapping (host addr == guest addr). Translation host = host_ptr + (guest_addr - guest_base), computed as integer arithmetic (never materialize a null-adjacent pointer). map()/access reject guest addresses below guest_base. Cranelift bakes the base-relative offset (byte-identical codegen when guest_base==0). New reserve_at(guest_base,span) mmap helper (MAP_FIXED_NOREPLACE|NORESERVE). Enables unemups4 PS4 HLE identity mapping (task-2 of that migration).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 HostRam/Memory carry guest_base (default 0); backing indexing subtracts it; map() and scalar/string/x87 access reject addr<guest_base
- [x] #2 Cranelift codegen bakes the base-relative offset; guest_base==0 emits byte-identical code (existing perf unchanged)
- [x] #3 reserve_at(guest_base,span) mmaps MAP_FIXED_NOREPLACE|NORESERVE and returns a HostRam with guest_base set
- [x] #4 Identity test: Reserved guest_base=0x10000, map 0x400000, write mov eax,42;hlt, run -> Exit::Hlt with RAX==42, and embedder-side *(0x400000 as *const u8)==0xB8, under both interpreter and cranelift
- [x] #5 Full suite green under both backends with guest_base=0 (zero behavior change)
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed on main (ff-merge of feat/guest-base-identity, 9b490a1) after review: full suite 369/369 green (--features unicorn, minus fuzz_robustness), clippy -D warnings clean, fmt clean — all three DoD gates verified on main. Review verdict: integer-arithmetic rebase (no null-adjacent pointers), byte-identical codegen at guest_base=0, append-only MemCtx ABI (offset 72, static-asserted), reserve_at asserts the kernel honored MAP_FIXED_NOREPLACE, below-base rejected in map() and trapped in interp+JIT (both tested). One documented non-blocker: SMC code_page table covers guest pages 0..extent>>12, so the top guest_base bytes of the space degrade to the same graceful no-op as >CODE_WINDOW code today.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
