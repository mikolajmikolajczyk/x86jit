# M7 — Multithreading + TSO

**Goal:** many `Vcpu`s over one shared `Vm`, with a memory model that keeps x86-TSO-assuming guests correct on weak hosts (ARM).

**Spec:** spec.md §11, §8.2.3, §9.1, §12 (M7), §16. **Prereq:** M4 (the `Vm`/`Vcpu` split and `CompiledPtr: Send + Sync` were prepared in M3/M4). Reach.

## Tasks

- [x] **M7-T1** — Run multiple `Vcpu`s on separate host threads over one `Arc<Vm>`. `Vm` is structurally `Send + Sync` (shared `Memory` + `RwLock` cache + `Send + Sync` backend); each thread owns its `Vcpu` and `run()` loop. `tests/threads.rs::parallel_squares_*` runs 8 vcpus over one `Arc<Vm>`, both backends. (§2, §11)
- [x] **M7-T2** — Cache synchronization: the shared `TranslationCache` fills under `RwLock` — the first thread to miss a block lifts+inserts it; concurrent misses may lift redundantly but insert the same valid block (translate-*at-least*-once, always correct). The hot loop is then reused via cache hit (interp) or chained link (JIT) rather than re-lifted. (§9, §11)
- [x] **M7-T3** — `CompiledPtr`/`CachedBlock`/`Vm` verified `Send + Sync` (and `Vcpu: Send`) by a compile-time assertion in `tests/threads.rs` — the M4 wrapper the M7 trap depends on. (§9.1, §16)
- [ ] **M7-T4** — `MemConsistency` tiers in codegen (§8.2.3): `Fast` = bare STR/LDR; `AcqRel` = STLR/LDAPR (RCpc, ARMv8.3 `FEAT_LRCPC`; LDAR fallback pre-8.3); `FullTso` = STR+`DMB ISH` / LDR+`DMB ISHLD`. No-op on x86 hosts (all tiers identical). Codegen applies the tier as a blanket to `MemOrder::None` accesses (lift stays tier-agnostic, §14). (§4.1, §8.2.3, §11)
- [ ] **M7-T4b** — Explicit sync is tier-independent: `lock`-prefixed ops / `xchg` → real atomics (CAS/LL-SC + full ordering), `mfence` → `DMB ISH`, in EVERY tier including `Fast`. (§8.2.3)
- [ ] **M7-T4c** — Tier is baked into compiled blocks: assert it's fixed per `Vm`, or if made switchable, the switch flushes the whole translation cache (don't key the cache by tier). (§8.2.3)

## Acceptance

- **M7-T5** — A multithreaded guest program that communicates through shared memory produces a **deterministic** result on a weak host (ARM) under `AcqRel` (and under `FullTso`) — the bug class that only appears multi-threaded is absent. Bonus: demonstrate the same program misbehaving under `Fast` (proves the tiers actually differ). (§12 M7, §11)

## Status

The **threading foundation is in place and tested** (T1–T3): multiple vcpus run in parallel over one `Arc<Vm>`, sharing memory and the translation cache, on both backends.

The **memory-ordering half (T4/T4b/T4c/T5) remains** and is deliberately gated on hardware: on the x86 host used for CI, all `MemConsistency` tiers emit identical code (native TSO), so weak-host reordering *cannot be reproduced or validated here*. Delivering it faithfully needs (a) an ARM host to test on, and (b) atomic-RMW lifting — `lock`-prefixed ALU ops, `xchg`, `cmpxchg`, `xadd` — which the lift currently treats as non-atomic (correct single-threaded, insufficient under contention). Both are their own body of work; do them when an ARM target and a contended-atomics workload are on the table.

## Exit criteria

Multiple guest threads run over one `Vm` (done) with correct cross-thread memory ordering (pending T4/T5, ARM host). This is the worst bug class to chase — lean on the deterministic multithreaded test and TSO barriers rather than luck.
