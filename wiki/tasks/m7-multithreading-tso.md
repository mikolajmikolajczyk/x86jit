# M7 — Multithreading + TSO

**Goal:** many `Vcpu`s over one shared `Vm`, with a memory model that keeps x86-TSO-assuming guests correct on weak hosts (ARM).

**Spec:** spec.md §11, §8.2.3, §9.1, §12 (M7), §16. **Prereq:** M4 (the `Vm`/`Vcpu` split and `CompiledPtr: Send + Sync` were prepared in M3/M4). Reach.

## Tasks

- [ ] **M7-T1** — Run multiple `Vcpu`s on separate host threads over one `Arc<Vm>` (shared memory + cache). (§2, §11)
- [ ] **M7-T2** — Cache synchronization: translate-once-per-block under `RwLock` (or a lock-free structure); consider a per-vcpu read cache + shared write path. (§9, §11)
- [ ] **M7-T3** — Verify `CompiledPtr`/`CachedBlock` are actually `Send + Sync` in the threaded cache (the M7 trap surfaces here if the M4 wrapper was skipped). (§9.1, §16)
- [ ] **M7-T4** — `MemConsistency` tiers in codegen (§8.2.3): `Fast` = bare STR/LDR; `AcqRel` = STLR/LDAPR (RCpc, ARMv8.3 `FEAT_LRCPC`; LDAR fallback pre-8.3); `FullTso` = STR+`DMB ISH` / LDR+`DMB ISHLD`. No-op on x86 hosts (all tiers identical). Codegen applies the tier as a blanket to `MemOrder::None` accesses (lift stays tier-agnostic, §14). (§4.1, §8.2.3, §11)
- [ ] **M7-T4b** — Explicit sync is tier-independent: `lock`-prefixed ops / `xchg` → real atomics (CAS/LL-SC + full ordering), `mfence` → `DMB ISH`, in EVERY tier including `Fast`. (§8.2.3)
- [ ] **M7-T4c** — Tier is baked into compiled blocks: assert it's fixed per `Vm`, or if made switchable, the switch flushes the whole translation cache (don't key the cache by tier). (§8.2.3)

## Acceptance

- **M7-T5** — A multithreaded guest program that communicates through shared memory produces a **deterministic** result on a weak host (ARM) under `AcqRel` (and under `FullTso`) — the bug class that only appears multi-threaded is absent. Bonus: demonstrate the same program misbehaving under `Fast` (proves the tiers actually differ). (§12 M7, §11)

## Exit criteria

Multiple guest threads run over one `Vm` with correct cross-thread memory ordering. This is the worst bug class to chase — lean on the deterministic multithreaded test and TSO barriers rather than luck.
