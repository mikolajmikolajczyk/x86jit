# M6 — Self-modifying code (SMC) invalidation

**Goal:** keep the translation cache consistent with the guest code buffer. A guest write to a page that has translated blocks invalidates them.

**Spec:** spec.md §10, §12 (M6), §16. **Prereq:** M4 (works with JIT arena). Reach; ignore until a real program rewrites its own `.text`.

## Tasks

- [x] **M6-T1** — Per-page "has translated code" tracking. `Memory` holds a `code_page` bitmap (one atomic bool per `CODE_PAGE_BITS`=4 KiB page); `resolve()` calls `mem.mark_code(start, len)` when a block is cached. (§10)
- [x] **M6-T2** — On write to a code page → remove affected cache entries. `Memory::write`/`write_bytes` call `note_write`, which records dirtied code pages; the dispatcher drains them via `Vm::handle_smc`, and `TranslationCache::invalidate_overlapping` drops every block whose guest span overlaps the page. *JIT-side "mark host code dead" (freeing arena code + patching chained link slots) remains deferred — see below.* (§10, §9.1)
- [x] **M6-T3** — On next execution → cache miss → re-lift from the changed bytes. `handle_smc` runs at the top of the dispatch loop, before `resolve()`, so the next fetch re-lifts. (§10)

## Acceptance

- [x] **M6-T4** — `tests/smc.rs`: the guest patches its own `.text` and re-executes the new instruction (interpreter); an embedder rewrites a cached block via `write_bytes` and both backends re-lift it; a write to a data page does not invalidate. (§10, testing.md §6)

## Deferred (accepted deviations, §10)

- **JIT-compiled guest stores bypass detection.** They write host RAM directly (§8.2.1), not through `Memory::write`, so a JIT block that stores onto its own code page isn't caught. Faithful coverage needs write-hooks in codegen or host page protection (mprotect + SIGSEGV). The interpreter path and all embedder writes (loader, syscall passthrough) are fully covered.
- **Stale chained link slots.** A compiled predecessor that chained (§12 M5) to an invalidated block keeps its baked entry pointer; reverse-edge invalidation is part of the same deferred "mark host code dead" step.
- **Same-block SMC** stays deferred per §10 (a running block finishes on its old bytes; re-lift takes effect on the next dispatch).

## Exit criteria

Guest self-modification is observed correctly (interpreter + embedder writes); cache never serves stale translations for those paths. Write the regression vector before fixing any SMC bug found in the field (testing.md §6.3).
