# M3 — Translation cache

**Goal:** stop re-lifting hot blocks. Cache keyed by guest address; dispatcher with hit/miss. Value is still an `IrBlock` (interpreter-from-cache), not yet JIT.

**Spec:** spec.md §9, §12 (M3); testing.md §11 (M3). **Prereq:** M2.

## Tasks

- [x] **M3-T1** — `TranslationCache` (`RwLock<HashMap<u64, CachedBlock>>`), `CachedBlock::Interpreted(Arc<IrBlock>)`, `CompiledPtr` type (`Send + Sync`, done early for M7). (§9.1)
- [x] **M3-T2** — Dispatcher hit/miss: `cache_get` clones the `CachedBlock` out (no lock guard held across execution — SMC safety); miss → lift → `materialize` → insert. (§9.2)
- [ ] **M3-T3** — Confirm the interpreter `materialize` arm wraps `Arc<IrBlock>` and the cache is actually populated on first execution of each block. (§8, §9)
- [ ] **M3-T4** — Hit/miss counters on the cache (for the test below and future stats). (§12 M3)
- [ ] **M3-T5b** — Seam-2 marker (§17.4): keep the cache key `u64` (guest addr) today, but leave the `SEAM` comment noting it would become `BlockKey { guest_addr, mode }` if processor modes were ever added. Don't build `BlockKey` now. (§17.4, §17.6)

## Acceptance

- **M3-T5** — A guest loop does not re-lift its body: run a small loop program, assert miss count == number of distinct blocks and hit count grows with iterations. (§12 M3, T§11 M3)

## Exit criteria

Repeated execution of the same guest addresses hits the cache instead of re-lifting. The `cache_get`-clones-out ownership model is in place — it's what keeps SMC (M6) and threading (M7) sound.
