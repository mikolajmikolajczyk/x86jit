---
id: TASK-273
title: >-
  dirty-tracking: watched-range take_dirty_ranges returns empty for
  guest-rewritten dynamic buffers
status: Done
assignee: []
created_date: '2026-07-19 22:09'
updated_date: '2026-07-20 07:17'
labels:
  - dirty-tracking
  - smc
  - embedder-unemups4
dependencies: []
priority: high
ordinal: 303000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The watched-range data dirty-tracking facility (x86jit-core/src/memory.rs: watch_range / note_watched_write / take_dirty_ranges, task-204/216/217) does not surface writes that guest code makes to a watched range. Observed from the unemups4 embedder (PS4 emulator): it watch_range()s a MonoGame DynamicVertexBuffer ring (~48000 B) and per-frame projection constant buffers (64 B), the guest rewrites them EVERY frame (managed SpriteBatch.SetData under interp+JIT), yet take_dirty_ranges() returns ZERO ranges every poll. The embedder's resource cache therefore served STALE bytes (66625 stale-hits measured over a run) — visually: Celeste title screen rendered frame-alternating garbage. Embedder worked around it by force-re-uploading all dynamic buffers (correct but not incremental); the proper fix is x86jit reporting these writes. Suspected: either the writes reach a path that skips note_watched_write / the JIT note_watched_store live watch_count gate, or watched pages are not re-armed after a take_dirty drain, or watch_range page-mapping misses the sub-page range. Needs a differential repro (watch a range, run guest stores into it via both interp and Cranelift tiers, assert take_dirty_ranges reports them).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A repro test watches a range, executes guest stores into it under BOTH interp and Cranelift-JIT tiers, and asserts take_dirty_ranges() reports the written page(s)
- [x] #2 The gap is root-caused (which write path / re-arm / mapping is missing) with evidence
- [x] #3 Fix lands: watched-range writes by guest code (both tiers) are reliably surfaced by take_dirty_ranges; the differential test passes
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Root cause found + fixed (not yet committed). Audited EVERY guest-memory write in the Cranelift backend (all gstore/store_guest/atomic_rmw/atomic_cas sites). Hooked paths were only scalar (emit_store), atomic RMW/CAS, and the rep-string helper. FIVE emitters wrote guest RAM with no note_watched_store:
  - emit_v_store (codegen/vector.rs:22) — movd/movq/movdqu/movaps, 4/8/16 B
  - emit_v_store_wide (vector.rs:69) — AVX/AVX-512 32/64 B
  - emit_v_extract_lane_wide_m (vector.rs:~272) — vextracti32x4 to memory
  - emit_v_store_half (vector.rs:~3170) — movhps/movlps
  - emit_call (codegen/control.rs:93) — the return-address push
That is exactly the symptom: MonoGame SetData / Mono memcpy copy the buffers with 16/32-byte SSE/AVX moves, which lower to VStore/VStoreWide, so take_dirty_ranges() saw nothing under the JIT while interp worked.

Two of the three hypotheses in the description are DISPROVED: (a) re-arm is fine — take_dirty_ranges only clears dirty_data + the flag, watch_page/watch_count are untouched; (b) sub-page mapping is fine — watch_range covers 64 B and 48000 B correctly (reporting is page-granular, an over-approximation in the safe direction). The live watch_count gate (task-217) also works; it just was not reached from the vector paths.

NOT affected (verified): masked stores VMaskStoreMem/VVecMaskStoreMem go through the shared fault-capable helpers that write via Memory (note_write); x87 + FXSAVE go through the x87/fxstate helpers likewise; real-mode IR ops never reach the JIT (unreachable! arm, mod.rs:1665); emit_ret_push writes the HOST shadow ring, not guest RAM. Known separate bypass: madvise(MADV_DONTNEED) host passthrough via host_ram_ptr — already task-234.

Fix: added note_watched_store to all five sites. Test: new jit_vector_and_call_stores_feed_watched_dirty_ranges_like_interp in x86jit-cranelift/tests/watch_dirty.rs, table-driven over movdqu / movd / vmovdqu ymm / vextracti32x4 / movhps / call-push, each asserting jit == interp dirty ranges. Verified it FAILS without the src fix (movdqu: left [] vs right [(16384, 4096)]).

The vextracti32x4 case uses the EVEX encoding because the VEX vextracti128 memory-destination form is not lifted (lift/mod.rs:1861 'mem dst deferred') — filed as TASK-274.

Verification: cargo nextest run -E 'not binary(fuzz_robustness)' -> 667/667 passed; clippy --all-targets --all-features -D warnings clean; cargo fmt --check clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
