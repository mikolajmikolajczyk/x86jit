# fast-dispatch — phased implementation plan

Branch: `feat/fast-dispatch`. Companion brief: [`fast-dispatch-brief.md`](fast-dispatch-brief.md).
Authored by Fable 5 planning agent; implemented by Opus 4.8.

## 0. Ground truth from the code (what the plan builds on)

1. **Every transfer already round-trips through the dispatcher.** Compiled blocks
   return to `Vcpu::run`'s inner chain loop (`x86jit-core/src/vm.rs`); `RET_CHAIN`
   re-enters compiled code via `ctx.next_entry` without a cache lookup. No
   code-to-code jump. "Fast dispatch" = replace `resolve()`'s `RwLock<HashMap>`
   lookup (plus two `fetch_add` counter bumps — cross-vcpu contention) with a
   slot/predictor check, staying inside the existing chain loop. Preserves
   `Blocks(n)` exactness: the inner loop ticks `blocks_run` and re-checks budget
   after every hop.
2. **Live latent SMC bug in link slots today.** `Vm::handle_smc` calls
   `cache.invalidate_overlapping` but ignores the returned victims; nothing clears
   the `Box<u64>` link slots owned by `JitBackend.slots`. Block A chains to B via a
   filled slot; embedder rewrites B via `write_bytes` (observed path — `smc.rs`);
   cache drops B; A's slot still holds B's old entry; next run of A returns
   `RET_CHAIN` into stale compiled code. Not memory-unsafe (`JITModule` never frees
   code before `Vm` drop) but semantically wrong, untested. **Any IBTC/return
   predictor inherits this — fixing it generically is the substrate for everything
   and the smallest correct first task.**
3. **Direct calls don't chain.** `IrOp::Call` returns `RET_CONTINUE` even when
   `target` is `Val::Imm`. A direct `call imm` = push return addr + direct jump —
   could use `chain_or_link` verbatim. Cheapest large win, zero new machinery.
4. **JIT-side SMC detection is a documented deferral**: JIT stores don't mark dirty
   code pages; `handle_smc` runs only in the outer loop. All invalidations today
   originate from interpreter stores or embedder `write_bytes`, both reaching the
   dispatcher outer loop before the next JIT entry. Plan preserves this; does not
   attempt JIT store-hook SMC.
5. **M7 threading is landed**: multiple vcpus share `Arc<Vm>`, run compiled code
   concurrently. Link-slot fills are documented racy-but-benign (aligned u64,
   same-value writes). Any pair-shaped cached pointer (target, entry) is NOT benign
   under tearing → forces the IBTC slot shape (D2).

## 1. Design decisions

### D1. SMC coherence: clear-on-invalidate + invalidation epoch for vcpu-local state
- **Backend-owned slots** (link, IBTC, call-continuation): when
  `invalidate_overlapping` returns non-empty victims, `Vm::handle_smc` calls new
  `Backend::invalidate_links(&self)` (default no-op; `JitBackend` zeroes every slot
  in `Jit.slots`). Over-invalidation (all slots) deliberate: invalidation rare, a
  cleared slot re-links via `RET_LINK` next traversal, avoids a reverse index. Also
  fixes the existing link-slot bug.
- **Vcpu-local predictor state** (R3 fast-resolve array, R5 ring): monotonic
  `epoch: AtomicU64` in `TranslationCache`, bumped in `invalidate_overlapping`.
  `run`'s outer loop compares a snapshot (one relaxed load/outer iter) and flushes
  vcpu-local caches on change. Covers cross-thread invalidation.
- **Shadow return stack stores no code pointers** (D4) — (guest return addr, slot
  address). Slot addresses stable for `Vm` life; contents covered by
  clear-on-invalidate. Structurally immune to dangling.
- Not per-use epoch checks (costs load+compare on every hit forever). Not
  indirect-through-cache (the lookup is the cost we remove).
- **Cross-thread window**: a vcpu mid-chain-loop when another thread invalidates
  can execute one stale hop before its next outer-loop epoch check. Matches
  pre-existing semantics; x86 cross-modifying code requires a serializing
  instruction on the executing CPU anyway. Document, don't solve here.
- **Hardening (R1)**: `Jit.slots` writes become `AtomicU64` relaxed stores on the
  Rust side (dispatcher fill + backend clear). Compiled-code loads stay plain
  (aligned u64 naturally atomic on x86-64).

### D2. IBTC shape: per-site, 1-way, single-u64 slot → pointer to immutable (target, entry) descriptor
- Per-site baked slot (like link slots), no hashing in generated code;
  per-site monomorphism common for `jmp reg`. Polymorphic returns handled by shadow
  stack (D4), not IBTC.
- 1-way to start; miss = today's baseline + one refill. N-way later if profiling shows need.
- **Slot = one u64** (existing `alloc_slot`, so R1's clear-all covers it). Value 0
  (empty) or pointer to immutable never-freed `[u64;2]` `{guest_target,
  compiled_entry}`. Raw pair mutated in place tears under M7 refill; single
  pointer-sized publish of immutable pair is race-free, cost one extra dependent
  load (3 loads: slot, target, entry). Descriptors in a `TranslationCache`-owned
  arena (`Mutex<Vec<Box<[u64;2]>>>`), never freed → no UAF mid-race.
- **Megamorphic guard**: per-`Vcpu` `HashMap<u64 slot, u32>` refill count; after ~8
  refills stop refilling (site pays baseline). Off hot path.

### D3. ABI: new `RET_IBTC_MISS = 7`; reuse `MemCtx.link_slot` for slot addr; hits reuse `RET_CHAIN`
- Hit: store computed RIP, compare descriptor target, store `next_entry =
  descriptor.entry`, `RET_CHAIN`. Dispatcher `RET_CHAIN` arm unchanged.
- Miss: `RET_IBTC_MISS` with slot addr in `MemCtx.link_slot`. New arm: resolve
  `cpu.rip`; if `Compiled`, alloc descriptor `{rip, entry}`, publish pointer into
  slot (atomic), `cur = entry`, continue inner loop; if `Interpreted`, break to
  outer loop like `RET_LINK` mixed-backend case.

### D4. Return prediction: per-Vcpu shadow ring of (return_addr, continuation_slot_addr)
- Each call site gets a **continuation slot** (link slot for the block at
  `return_addr`, via `alloc_slot`). Compiled `Call` pushes `(return_addr,
  cont_slot_addr)` onto the ring (plus unchanged guest-stack push).
- Compiled `Ret`: real guest pop / RSP update as today, then pop ring, compare
  actual popped target to predicted:
  - match, `*cont_slot != 0` → `next_entry = *cont_slot`, `RET_CHAIN`.
  - match, `*cont_slot == 0` → `link_slot = cont_slot`, `RET_LINK` (existing arm fills).
  - mismatch/empty → `RET_CONTINUE` (today). The wrong-prediction slow path.
- **Correctness**: prediction followed only when `predicted_addr == actual target`
  AND entry came from a slot that only ever holds the compiled entry for that exact
  guest address (filled by `resolve`). Ring is purely a candidate-finding
  heuristic; correctness never depends on ring integrity. Overflow (fixed 64-entry
  ring, wrap-overwrite), underflow, stale, cross-`run()` reuse → only a miss, never
  a wrong transfer. No RSP tracking needed.
- **SMC**: cont-slots cleared by R1; ring holds only guest addrs + stable slot
  addrs. Belt-and-braces: reset ring `sp` on epoch check.
- **Location/ABI**: `#[repr(C)] RetStack { sp: u64, entries: [[u64;2]; 64] }` field
  in `Vcpu` (persists across `run()`). New `MemCtx.ret_stack` at offset 64
  (append-only), set by `run`; `run_compiled` gets scratch `RetStack` (never null).
  New `MEMCTX_RET_STACK`, `RETSTACK_*` offset consts + extend
  `memctx_offsets_match_layout`.

### D5. AOT / persistent cache: out of this track — deferred
- Structurally blocked: compiled code bakes run-specific absolute addresses (slot
  heap addrs, helper fn addrs via `JITBuilder::symbol`, `is_pic=false`). Persisting
  needs table-relative/relocatable everything — codegen rearchitecture.
- This track churns exactly that machinery. Sequence AOT after fast dispatch stabilizes.
- Orthogonal: AOT attacks compile cost; this track attacks per-transfer dispatch cost.
- Prereqs to record: (1) slot-table indirection, (2) helper-table indirection, (3)
  `is_pic=true` + retained relocations, (4) cache key = guest-byte hash + codegen
  version + tier, (5) cross-run invalidation.

## 2. Phases

Each independently landable, whole suite green (differential/fuzz/corpus vs
Unicorn; whole-program busybox/sqlite/lua/python/gzip/djpeg/glibc/dynamic;
smc/threads/mt/tso; jit/superblock/cache).

### R1 — Link-slot SMC coherence (correctness substrate; first task)
- `vm.rs`: `Backend::invalidate_links(&self) {}` default; `handle_smc` captures
  victims, calls `invalidate_links` if non-empty.
- `cache.rs`: `epoch: AtomicU64`, bumped (Release) in `invalidate_overlapping`; `pub fn epoch()`.
- `lib.rs`: `JitBackend::invalidate_links` zeroes every slot; slot writes → AtomicU64 relaxed.
- `smc.rs`: acceptance test `stale_link_slot_cleared_on_invalidation` (see §4).
- Perf: none steady-state. Pure correctness.

### R2 — Chain direct calls (`call imm` → `chain_or_link`)
- `codegen.rs` `IrOp::Call`: `Val::Imm` target → after push/RSP/RIP, alloc slot,
  `chain_or_link(slot)` instead of `ret(RET_CONTINUE)`. `Val::Temp` unchanged (R4).
- Perf: likely largest single win — most calls direct.

### R3 — Per-vcpu fast-resolve cache (dispatcher-side, no ABI change)
- `vm.rs`: `Vcpu` direct-mapped `[(u64 rip, CompiledPtr); 1024]` + epoch snapshot.
  Outer loop: after `handle_smc`, if epoch changed clear array (+ R5 ring). Probe
  before `resolve`; hit → compiled inner loop; miss → resolve+install (Compiled
  only). Install on `RET_LINK`/`RET_IBTC_MISS` resolves too.
- Perf: halves residual dispatch for ret/indirect; removes counter contention.

### R4 — IBTC for indirect jmp/call (baked per-site slots)
- `jit_abi.rs`: `RET_IBTC_MISS = 7`.
- `codegen.rs`: `Jump{Temp}` + `Call{Temp}` → `ibtc_or_miss(slot, target)` helper.
- `cache.rs`: descriptor arena + `alloc_ibtc_descriptor`; `ibtc_filled` counter.
- `vm.rs`: `RET_IBTC_MISS` arm + megamorphic refill cap.

### R5 — Shadow return stack
- `jit_abi.rs`: `MemCtx.ret_stack` offset 64; `RetStack` type; extend layout test;
  `run_compiled` scratch.
- `vm.rs`: `Vcpu.ret_stack`; `run` sets `ctx.ret_stack`; ring `sp` reset on epoch flush.
- `codegen.rs`: `Call` (both) push ring; `Ret` pop+compare+three-way exit.
- Perf: second-largest after R2.

### R6 — Measurement and counters polish
- Committed call/ret microbench (recursive-fib), `#[ignore]` timing.
- Wire `ibtc_filled`, fast-resolve hits, ret-prediction fires into stats.
- Before/after busybox/sqlite/lua/CPython in PR; confirm no call-light regression.

### Deferred — AOT / persistent cache (D5). Record prereqs in `wiki/tasks/`.

## 3. ABI change summary

| Change | Where | Phase |
|---|---|---|
| `Backend::invalidate_links` default no-op | `vm.rs` | R1 |
| `TranslationCache.epoch: AtomicU64` | `cache.rs` | R1 |
| Slot writes atomic (Rust side, no codegen change) | `vm.rs`, `lib.rs` | R1 |
| `RET_IBTC_MISS = 7`; `MemCtx.link_slot` reused for slot addr | `jit_abi.rs`, `vm.rs` | R4 |
| IBTC descriptor arena + `ibtc_filled` | `cache.rs` | R4 |
| `MemCtx.ret_stack` offset 64 + `RetStack` + `RETSTACK_*` + layout test + scratch | `jit_abi.rs` | R5 |
| `Vcpu.ret_stack: RetStack` | `vm.rs` | R5 |

`CompiledFn` signature never changes; `MemCtx` grows append-only.

## 4. First task: R1 acceptance test

`x86jit-tests/tests/smc.rs` (JIT): `stale_link_slot_cleared_on_invalidation` —
1. `MAIN`: block ending in direct `jmp TARGET`; `TARGET`: `mov eax, 1; hlt`. Run
   from `MAIN` → chains (slot filled via `RET_LINK`), `rax == 1`.
2. Embedder rewrites `TARGET` via `vm.write_bytes` to `mov eax, 42; hlt`.
3. Run from `MAIN` again (fresh vcpu). Without fix: stale slot → `RET_CHAIN` → `rax
   == 1`. With fix: slots cleared, edge re-links, `rax == 42`. Assert `rax == 42`
   and `cache.misses()` increased.

Write test first, confirm it fails on current tree, then land the fix. Full suite green.

## 5. Delivered (all phases landed)

All of R1–R6 are implemented and committed on `feat/fast-dispatch`; the
full suite (differential/fuzz/corpus vs Unicorn, whole-program
busybox/sqlite/lua/CPython/gzip/djpeg/glibc/dynamic, smc/threads/mt/tso,
jit/superblock/cache) is green on x86, and clippy is clean.

| Phase | Commit subject | New tests |
|---|---|---|
| R1 | fix: clear link slots on SMC invalidation | `stale_link_slot_cleared_on_invalidation` |
| R2 | perf: chain direct calls through a link slot | `direct_call_chains_the_callee_edge`, `call_loop_budget_stops_at_the_same_state` |
| R3 | perf: per-vcpu fast-resolve cache | `fast_resolve_cache_flushes_on_invalidation` |
| R4 | perf: indirect-branch target cache | `monomorphic_indirect_jump_fills_and_chains_via_ibtc`, `polymorphic_indirect_jump_matches_interpreter`, `stale_ibtc_descriptor_cleared_on_invalidation` |
| R5 | perf: shadow return stack | `return_prediction_chains_the_ret_edge`, `overwritten_return_address_is_not_mispredicted`, `deep_recursion_beyond_ring_wraps_correctly` |
| R6 | perf: call/ret microbenchmark + fast-hit counter | `fast_dispatch_call_bench` (`#[ignore]` timing) |

**Measured `fib(32)` (pure call/ret), one dev machine:**

- pre-track baseline: **340.9 ms** JIT, chained = 7.0M (only loop back-edges chain)
- R1–R5: **110.4 ms** JIT, chained = 21.1M, misses = 7, fast_hits = 0
- **→ 3.09× on call/ret-heavy code**, 10.7× over the interpreter.

The `chained` tripling is the mechanism: the direct-call edge (R2) and the
predicted return edge (R5) now chain alongside the back-edge, so the whole
recursion stays in the compiled inner chain loop — it never falls back to the
outer-loop probe (`fast_hits = 0`) and re-lifts stay at the 7 cold misses. The
per-vcpu fast-resolve cache (R3) and IBTC (R4) carry the transfers chaining can't
(cross-`run()` re-entry, `jmp reg`); they don't fire on this all-direct-call
workload but cover vtable/switch/computed-goto code.
