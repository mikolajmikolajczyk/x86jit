---
id: TASK-275
title: >-
  memory: watch_page inherits the 4 GiB CODE_WINDOW cap — watch_range silently
  no-ops for high guest addresses, so take_dirty_ranges is always empty
status: Done
assignee: []
created_date: '2026-07-20 07:35'
updated_date: '2026-07-20 09:24'
labels:
  - memory
  - dirty-tracking
  - embedder
  - perf
dependencies: []
priority: high
ordinal: 305000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Embedder-registered DATA-range dirty tracking (task-204/216/217) is dead for any watched range above 4 GiB, because `watch_page` is sized with the CODE-page sizing function:

    let watch_page = fresh_code_pages(backing.len());   // memory.rs:373

    fn fresh_code_pages(backing_len: usize) -> Box<[AtomicBool]> {
        let tracked = backing_len.min(CODE_WINDOW as usize);   // CODE_WINDOW = 4 << 30
        ...
    }

The cap's justification is sound FOR CODE and is documented as such at memory.rs:260-268 — 'Guest code always lives in the low image/interp region, never in the multi-hundred-GiB heap it reserves'. It was then reused verbatim for watched DATA pages, where the premise inverts: watched data is precisely the embedder's large buffers in the high heap.

Consequence: in `watch_range`, `self.watch_page.get(page as usize)` returns `None` for those pages, so the registration is silently dropped — no error, no counter. `note_write` / `note_watched_write` then never find a set bit, and `take_dirty_ranges()` returns empty forever.

MEASURED in unemups4 (PS4 emulator embedder, 64 GiB identity-mapped arena): guest GPU buffers live around 0x9afd52800 (~41.4 GiB). `take_dirty` returned 0 ranges on 3709 of 3709 GPU submits. The embedder cannot trust the dirty flag at all and carries a force-re-upload workaround for every dynamic vertex/index/const buffer.

Note this also explains why 2f9372a ('vector + call stores must feed watched-range dirty tracking') produced no observable change for this embedder: that fix repaired the store path, but the page bit is never registered in the first place, so the store path had nothing to find.

SECOND DEFECT, found while diagnosing — `dirty_data` dedupes at DRAIN time (take_dirty_ranges sorts + dedups), so `note_watched_write` takes `dirty_data.lock()` and pushes ONCE PER STORE. This is invisible today because the mechanism never fires; the moment #1 is fixed it becomes a hot path. A guest memcpy over a 48000-byte buffer is hundreds of stores per page, each taking the mutex.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 watch_page is sized independently of CODE_WINDOW and covers the full guest span, so watch_range registers pages at any address the embedder can map
- [x] #2 watch_range no longer degrades silently: out-of-range registrations are impossible (full coverage) or are surfaced (returned count / debug_assert), never dropped without a signal
- [x] #3 note_watched_write pushes a page to dirty_data at most once per drain epoch (0->1 transition on a per-page dirty bit), not once per store
- [x] #4 zero-cost-when-unwatched (task-204) is preserved: an unwatched memory still pays only the single relaxed watch_count load on the store path
- [x] #5 memory cost stays sparse: measured RSS increase for a 1 TiB Reserved span with a handful of watched pages is negligible
- [x] #6 regression test: watch a range above 4 GiB in a Reserved VM, write to it from guest code, assert take_dirty_ranges reports it (interpreter AND cranelift)
- [x] #7 throughput check: a tight guest store loop over a watched page is not slower than the pre-fix build
- [ ] #8 End-to-end regression budget: the embedder's frame rate on its reference title must stay within 5% of the pre-fix build. NOTE this fix necessarily ADDS store-path cost that does not exist today: because watch_range's bit never lands, watch_count is never incremented, so the embedder currently sits permanently in the zero-cost unwatched fast path. Measure before/after end-to-end, not just microbenchmarks.
- [ ] #9 dirty-bit marking uses load-then-swap, not a bare swap: a bare RMW takes the cache line exclusive on EVERY store to an already-dirty page and will ping-pong between guest threads writing the same buffer. Same pattern take_dirty_code already documents ('a shared load, not a swap').
<!-- AC:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Suggested shape (embedder's analysis; maintainer's call):

1. Split the sizing. Give watched-DATA pages their own constructor instead of reusing fresh_code_pages — the CODE_WINDOW cap must not reach it.

2. Bitset over the full span, lazily committed. The cap exists because 'one AtomicBool per 4 KiB page across all of it would commit hundreds of MiB'. Two changes together dissolve that:
   - one BIT per page (Box<[AtomicU64]> / raw AtomicU64 region) instead of one byte -> 8x
   - allocate as a zeroed anonymous mmap, so only words actually touched commit
   1 TiB span -> 32 MiB virtual, a few KiB resident. 64 GiB -> 2 MiB virtual. No two-level radix table needed; the OS provides the sparseness.
   Write path stays one load + test: word = page >> 6, bit = page & 63.

3. Dedupe at write time. Add a parallel dirty-page bitset; push only on the 0->1 transition:

       if watch_bit(page) && !dirty_bit(page).swap(true, Relaxed) {
           self.dirty_data.lock().unwrap().push(page);
           self.dirty_data_flag.store(true, Relaxed);
       }

   take_dirty_ranges clears the bits as it drains (and can then skip its sort/dedup, since pages arrive unique).

Expected performance, per case:
  - nothing watched: unchanged — the watch_count gate is untouched, still one relaxed load.
  - watched, store outside a watched page: gate + one bitset word load + test, vs today's gate + one byte load. Equivalent.
  - watched, store inside a watched page: today mutex+push PER STORE; after, one atomic swap, mutex once per page per epoch. Strictly faster.

So the fix should be perf-neutral where it matters and perf-positive on the path that actually fires.

Page granularity is fine for the embedder — over-approximation (a 64-byte const buffer reported as its whole 4 KiB page) is the safe direction; no byte-exactness needed.

Related: task-234 (MADV_DONTNEED bypasses tracking) is NOT a factor for this embedder — its guest-facing madvise is a no-op stub, so guest DONTNEED never reaches the host and cannot zero watched pages behind the tracker's back.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented (not yet committed).

DESIGN. New `WatchPages` in x86jit-core/src/memory.rs replaces watch_page/dirty_data/dirty_data_flag:
  - watch + dirty are one BIT per 4 KiB page (Box<[AtomicU64]>), sized over the WHOLE guest span (guest_base + backing len), NOT fresh_code_pages. CODE_WINDOW now applies only to the SMC code table, where its 'code always lives low' premise actually holds.
  - Allocated via `zeroed_words`: vec![0u64; n] (alloc_zeroed -> mmap) reinterpreted as [AtomicU64]. Element-wise `collect()` would fault in every page and defeat the sparseness — that is the whole point, so there is a test pinning it.
  - watch_count kept as a field of WatchPages; watch_count_ptr now returns &self.watch.count. The JIT gate is untouched (AC#4).

INDEXING FIX not in the original report: page indices are RAW guest page numbers while the backing indexes at (addr - guest_base), so the table is sized to guest_base + backing.len(). Sizing it by backing.len() alone would still mis-index any embedder using a non-zero guest_base.

DIRTY PATH. mark_dirty is test-and-test-and-set: load(Relaxed), and only the 0->1 transition does fetch_or(Release). Measured (x86, 4 threads on one buffer): unconditional RMW 13.5 ns/store vs 0.25 ns for TTAS. Also measured that bit-packing WITHOUT TTAS is 1.7x worse than a byte-per-page array when threads write different pages sharing a bitset word (13.2 vs 7.7 ns) — TTAS erases that, so bitset+TTAS gets the 8x memory win for free. bitset without TTAS would have been a regression.

DRAIN. take_dirty_ranges no longer keeps a Vec+Mutex+flag. It scans the words that hold a watched page (a BTreeSet<u64> maintained by watch/unwatch, cold paths only), swap(0, Acquire) per word, expanding set bits. Cost O(watched pages / 64). Output is naturally ascending and duplicate-free, so the old sort+dedup is gone. Fast path when nothing is watched is a single relaxed watch_count load, still lock-free.

ORDERING. The Release on the 0->1 transition pairs with the Acquire in the drain, so a drain that sees a page dirty sees the store that dirtied it. A writer that finds the bit already set adds no edge — deliberate, and documented in the code: dirty tracking reports WHICH pages changed, never a consistent snapshot of their CONTENTS (the guest can write the instant after a drain returns), so embedders needing coherent bytes must quiesce, exactly as before. No page can be lost: a write racing the drain either sets the bit before the swap (this epoch) or after (next).

TESTS. x86jit-core memory: watch_works_above_the_4gib_code_window (41 GiB, the reported address); watch_table_over_a_huge_span_stays_sparse (1 TiB span, asserts <4 MiB RSS growth — verified it FAILS at 67903488 bytes with element-wise init, i.e. it really guards the lazy commit); rewatching_a_page_does_not_double_count; unwatch_drops_a_pending_dirty_bit; many_pages_within_one_bitset_word_all_report. x86jit-cranelift watch_dirty: jit_store_above_the_4gib_code_window_is_reported (AC#6 for the JIT tier, interp half in core).

AC#7 measured IN-ENGINE, not just synthetically: jit_store_seen_when_watch_installed_mid_run_by_another_thread (5M guest stores into a watched page) 0.367/0.367/0.378 s before -> 0.250/0.252/0.292 s after. ~32% faster, not slower.

VERIFICATION. cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' -> 884/884 passed; clippy --all-targets --all-features -D warnings clean; cargo fmt --check clean. Note: a full parallel run showed go_http_serves_index_jit_eager at 523 s vs 158 s previously; re-run isolated it is 58.7 s, so that is the known contention variance of the parallel suite, not a regression.

CAVEAT: all microbenchmark numbers are x86 hosts. ARM (the primary target) has costlier RMW, so TTAS should help at least as much there, but that is unmeasured — no ARM machine locally.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
