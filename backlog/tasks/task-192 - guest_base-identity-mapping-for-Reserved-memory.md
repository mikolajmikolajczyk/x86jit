---
id: TASK-192
title: guest_base identity mapping for Reserved memory
status: In Progress
assignee: []
created_date: '2026-07-09 15:19'
updated_date: '2026-07-09 15:57'
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
Landed on branch feat/guest-base-identity. Design: HostRam.guest_base -> Backing.guest_base -> Memory.guest_base (cached); all backing indexing via host_off(addr) = (addr - guest_base) as usize (integer arithmetic, no null-adjacent pointer); map() rejects guest_addr < guest_base; from_host_ram asserts guest_base <= span and (span - guest_base) <= ram.len. JIT: guest_base baked as a compile-time constant threaded like the mmio window (Backend::materialize/materialize_region/TierUpRequest); checked_addr emits below-base reject + isub rebase ONLY when non-zero, so guest_base==0 codegen is byte-identical. MemCtx grew guest_base at offset 72 (append-only ABI); string/x87/fxstate helpers rebase via RawStrMem/RawFpMem.guest_base. reserve_at(guest_base, span) in x86jit-linux mmaps [guest_base, span) MAP_FIXED_NOREPLACE|NORESERVE and asserts the kernel honored the address. deep_copy of a non-zero-based memory returns None (can't re-home into a zero-based boxed child; identity embedders don't fork). SMC page numbering stays guest-address-relative (dispatcher round-trips page<<12 to guest RIPs); CODE_WINDOW covers it. reprotect converted to backing-offset space. Tests: x86jit-tests/tests/guest_base.rs (identity + below-base traps, both backends), hostmem reserve_at unit test, core memory unit tests. Full suite 305/305 green; clippy -D warnings clean. Not Done until it lands on main.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
