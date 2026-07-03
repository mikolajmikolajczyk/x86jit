# M6 — Self-modifying code (SMC) invalidation

**Goal:** keep the translation cache consistent with the guest code buffer. A guest write to a page that has translated blocks invalidates them.

**Spec:** spec.md §10, §12 (M6), §16. **Prereq:** M4 (works with JIT arena). Reach; ignore until a real program rewrites its own `.text`.

## Tasks

- [ ] **M6-T1** — Per-page "has translated code" tracking (a bit per guest page for pages that back cached blocks). (§10)
- [ ] **M6-T2** — On write to a code page → remove affected cache entries; for the JIT, also mark the host code dead (arena memory reclaimed later or via a recycling mechanism — late optimization). (§10, §9.1)
- [ ] **M6-T3** — On next execution → cache miss → re-lift from the changed bytes. (§10)

## Acceptance

- **M6-T4** — Vector/test: a program writes new bytes over an already-executed block, then jumps into it; the engine executes the *new* instructions, matching the oracle. (§10, testing.md §6)

## Exit criteria

Guest self-modification is observed correctly; cache never serves stale translations. Write the regression vector before fixing any SMC bug found in the field (testing.md §6.3).
