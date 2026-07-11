---
id: TASK-216
title: Watched-range dirty tracking must cover JIT'd (Cranelift) guest stores
status: Done
assignee: []
created_date: '2026-07-11 16:12'
updated_date: '2026-07-11 17:06'
labels:
  - perf
dependencies: []
ordinal: 245000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-204 added embedder watched-data-range dirty tracking (watch_range/unwatch_range/take_dirty_ranges) but the watch check lives ONLY in Memory::note_write (x86jit-core/src/memory.rs:454-478), which fires solely on interpreter-executed stores + embedder write_bytes. The Cranelift JIT INLINES guest stores as raw host stores with NO watch/SMC check: emit_store → checked_addr (bounds+MMIO only) → store_guest → plain gstore (x86jit-cranelift/src/codegen/memory.rs:12-18, codegen/mod.rs ~1821-1958); the rep-movs string helper uses the same bounds-only view. The JIT's own comment says it (x86jit-cranelift/src/lib.rs:87 'inlined stores skip SMC/region handling, deferred §10'). CONSEQUENCE for the unemups4 embedder: it runs the JIT by default with tier-up after ~50 execs, so any hot loop writing dynamic vertex/constant/texture data — exactly the workload its GPU resource cache invalidates on — becomes INVISIBLE to take_dirty_ranges the moment it tiers up. Confirmed absent at both the currently-pinned rev (26bc5ec) and HEAD (47b7e6f): cranelift codegen has no watch handling; task-204's ACs never exercised JIT'd stores. This blocks unemups4 phase-4 task-48 (its AC #2 = 'JIT'd guest code writing a watched range → take_dirty_ranges reports it' cannot pass) and downstream cache correctness (silently stale GPU buffers/textures). Requested by the unemups4 embedder (GPU resource cache, doc-4 §8.3).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A guest store executed by the CRANELIFT JIT into a watched range is reported by take_dirty_ranges (not just interpreter/write_bytes stores) — regression test that runs the same store under both interp and JIT and asserts identical dirty output
- [x] #2 String/block helpers (rep movs and any bulk-store path) that write a watched range are covered too
- [x] #3 Zero measurable overhead on the JIT store path when no ranges are watched (watch_count-gated inline check, mirroring the existing note_write one-relaxed-load gate)
- [x] #4 Unicorn differential corpus green; interp-vs-JIT parity on dirty output; clippy -D warnings + fmt clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE. The Cranelift JIT's inlined guest stores now feed watched-range dirty tracking.

Mechanism (mirrors Memory::note_write's watch half, WITHOUT the SMC code-page check — JIT-side SMC stays deferred §10):
- MemCtx grew two append-only fields (jit_abi.rs): watch_count (u64 snapshot of Memory::watch_count at run start, MEMCTX_WATCH_COUNT=88) and mem_self (*const Memory, MEMCTX_MEM_SELF=96). for_memory populates both; offset asserts added. Snapshot is correct because embedders watch/unwatch at frame boundaries (between run() calls); the set is stable within a run.
- Memory::note_watched_write(addr,len) = the watch-only recording; Memory::watch_count_snapshot() for the gate (memory.rs).
- codegen Translator::note_watched_store(guest_addr,size): loads MEMCTX_WATCH_COUNT (one relaxed load), brif !=0 -> call note_watched_write_helper(mem_self, addr, len) else skip. Zero overhead when nothing watched (one load + never-taken branch), mirroring note_write's gate (AC#3). Called from emit_store, emit_atomic_rmw, emit_atomic_cas (codegen/memory.rs).
- rep movs/stos: string_helper (lib.rs) now snapshots RDI (gpr[7]) around string_run and, when watched, marks the destination span [min,max)+elem via note_watched_write — conservative over-approx by <=1 element, safe for dirty tracking (AC#2). movs/stos are the only string ops that write.
- Helper note_watched_write_helper + x86jit_note_watch symbol + note_watch_sig=params(3,false) registered (lib.rs).

Tests: x86jit-cranelift/tests/watch_dirty.rs — runs the same store (mov [rdi],eax) and rep-stosb under interp and under a JIT-compiled block (tier_up_after(Some(0)) warms up+compiles, then a 2nd run executes the compiled block) and asserts identical dirty output. VERIFIED the test catches the bug: with the emit_store hook removed it fails (JIT dirty [] vs interp [(0x4000,0x1000)]).

Verification: cargo nextest --features unicorn -E 'not binary(fuzz_robustness)' = 566 passed. clippy --all-targets --all-features -D warnings clean. fmt --check clean. aarch64 cross-check clean. ABI offset test green.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
