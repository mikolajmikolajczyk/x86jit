# fast-dispatch — planning brief

Input for designing the next JIT-perf work, adopting fast-dispatch techniques for
x86-64→ARM64 translation. Branch: `feat/fast-dispatch`.

## Goal

Cut the per-transfer dispatcher cost for the control-flow edges we do **not** yet
chain: **indirect jumps (`jmp reg`), calls, and returns**. Today only *static*
direct edges (`jmp`/`jcc` to an immediate address) are chained via link slots;
every `call`, `ret`, and indirect `jmp` returns to the dispatcher, which does a
`HashMap` lookup (`cache.get(rip)`) and an indirect call back into compiled code.
Call/return-heavy guest code (function-call trees, vtables, `switch`) pays this on
every transfer. fast-dispatch gets much of its speed from caching these.

Two fast-dispatch techniques are the target:

1. **Indirect-branch target cache (IBTC).** Per indirect-branch *site*, remember
   the last runtime target address and its compiled entry; on the next execution,
   if the computed target equals the cached one, jump straight to the cached entry
   without a dispatcher lookup. (Generalizes our static link slots to a
   value-keyed slot.)
2. **Return-address prediction (a shadow return stack).** On `call`, push the
   return address (and its compiled entry) onto a per-vcpu prediction stack; on
   `ret`, if the popped/return target matches, jump to the predicted entry
   directly. Returns are the most common indirect branch and the most
   predictable.

Optionally in scope if the plan judges it worthwhile — **an AOT / persistent
translation cache** (compile once, key by guest-byte hash, persist to disk / reuse
across runs) to amortize compile cost. This is the structural answer to the
superblock compile-cost problem (superblocks are opt-in today precisely because
the region compile doesn't amortize on short runs). Larger and riskier
(serializing Cranelift output / re-linkable code), so the plan should decide
whether it belongs here or is a separate track.

## Current architecture (what fast dispatch must integrate with)

- **Dispatcher** (`x86jit-core/src/vm.rs::Vcpu::run`): a `loop` that resolves the
  block at `cpu.rip`, runs it, and reacts to the returned ABI code. Compiled
  blocks run in an inner **chain loop**:
  - `RET_CONTINUE (0)` → break to the outer loop (re-resolve `rip`). This is what
    `call`/`ret`/indirect-`jmp` return today → a `cache.get(rip)` lookup follows.
  - `RET_CHAIN (4)` → `MemCtx.next_entry` holds the next compiled entry; the inner
    loop jumps straight there (no lookup). Set by a *filled* link slot.
  - `RET_LINK (5)` → `MemCtx.link_slot` holds a slot address; the dispatcher
    resolves `rip`, writes the entry into the slot (so it chains next time), and
    continues. Cold direct edge.
  - `RET_SYSCALL/HLT/UNMAPPED/EXCEPTION` → exit `run()`.
  - Budget (`Blocks(n)`) is charged per block via the fuel field (superblocks
    M5-T3); a compiled region decrements it, a single block leaves it and the
    dispatcher charges 1.
- **Static edge chaining** (`x86jit-cranelift/src/codegen.rs`): a direct
  `Jump{Imm}` / both arms of `Branch` store RIP and call `chain_or_link(slot)` —
  it loads the slot (a `Box<u64>` owned by `JitBackend`, address baked as a
  constant); if non-zero it returns `RET_CHAIN` with `next_entry = *slot`, else
  `RET_LINK` with `link_slot = &slot`. The dispatcher fills the slot on the first
  traversal. Link slots are single-threaded writes today (atomics deferred to M7).
- **Call / Ret / indirect jump** (`codegen.rs`): all three store RIP and
  `ret(RET_CONTINUE)` — no chaining. `Call` also pushes the return address to the
  guest stack (a guest memory store) and sets RSP; `Ret` pops it. Indirect
  `Jump{Val::Temp}` stores the computed RIP and returns.
- **Cache** (`x86jit-core/src/cache.rs`): `TranslationCache` — `map: RwLock<HashMap
  <u64, CachedBlock>>` keyed by guest entry address, plus a `spans` map (one or
  more `(start,len)` per unit) for SMC invalidation, and hit/miss/chained/regions
  counters. `CachedBlock::Compiled { entry: CompiledPtr, guest_len }`.
- **ABI** (`x86jit-core/src/jit_abi.rs`): compiled fn `extern "C" fn(cpu: *mut u8,
  mem: *mut u8) -> u64`. `MemCtx { base, size, fault_addr, fault_size,
  fault_access, next_entry, link_slot, fuel }` `#[repr(C)]` (offsets 0..56). The
  `CpuState` field offsets are in `CpuOffsets`.
- **Per-vcpu state** (`Vcpu`): `cpu: CpuState`, `pending_mmio`. Each guest thread
  has its own `Vcpu`; the `Vm` (cache + guest RAM + backend) is shared behind
  `Arc` (M7).

## Hard invariants any fast-dispatch scheme must not break

1. **interp == JIT == Unicorn** — the differential/fuzz/corpus oracles compare full
   CPU state at `hlt`/budget exhaustion. A predicted/cached transfer must land in
   exactly the state a re-resolve would, and must NOT change *which* guest block
   runs for a given budget.
2. **Preemption / `Blocks(n)`** (§9.2) — the budget is counted in guest blocks. A
   chained/predicted transfer must still tick the budget and be able to yield
   `BudgetExhausted` at the same block the interpreter would. (Today the inner
   chain loop re-checks the budget each hop.)
3. **SMC (§10 / M6)** — a store onto a code page invalidates the cached block(s).
   An IBTC entry or a return-stack prediction that points at a compiled entry must
   be invalidated (or safely re-validated) when that entry is dropped, or it will
   jump into freed/stale code. This is the sharp edge: cached *pointers* to
   compiled code outlive a single dispatch and must be kept coherent with the
   cache. (Static link slots have the same issue today — note how/whether they are
   currently invalidated on SMC, and match or improve it.)
4. **M7 threading** — the cache and guest RAM are shared across vcpus behind `Arc`;
   compiled code is read-only and executable from any thread. Any new shared
   mutable state (an IBTC table, a global prediction structure) needs the same
   care; per-vcpu structures (a return stack) avoid it. Link-slot writes are
   single-threaded today (documented deferral).
5. **RIP-retry / instruction atomicity** — on a trap, guest state must be
   consistent with "up to, excluding, the faulting instruction." A prediction that
   guesses wrong must fall back to a correct re-resolve, never commit a wrong
   transfer.
6. **Correctness of a wrong prediction** — the whole point of a *predictor* is that
   a miss is cheap and correct (fall back to the dispatcher). Every scheme must
   have a validated slow path: if the cached target ≠ the real target, or the
   entry was invalidated, re-resolve.

## Design space / open questions for the plan

- **IBTC shape**: where does the per-site cache live — a slot baked per indirect
  site (like link slots, one `Box<(u64 target, u64 entry)>` per site), checked in
  compiled code (compare computed target to cached target; hit → jump, miss → a
  new RET code that carries the target so the dispatcher fills the slot)? How many
  entries per site (1-way vs N-way)? A new `RET_*` code, or reuse `RET_LINK` with
  the target in `MemCtx`?
- **Return prediction**: a per-vcpu return-address stack (pushed on `call`, popped
  on `ret`). How is it kept in sync with the guest stack (RSP) so a mispredict
  (setjmp/longjmp, exceptions, hand-rolled stack munging) is detected and falls
  back? Does the prediction stack store (return_addr, compiled_entry) or just the
  addr (then an IBTC/lookup resolves the entry)? Where does it live — in `Vcpu`
  (needs an ABI channel: a pointer in `MemCtx`, or grow the ABI)?
- **SMC coherence for cached pointers** — the critical correctness question. When
  `invalidate_overlapping` drops a compiled unit, every IBTC slot and every
  return-stack entry pointing at it becomes dangling. Options: version/epoch
  counter checked on use; clear all predictors on any invalidation; indirect
  through the cache (store the target *address*, re-lookup its entry — cheaper than
  a full dispatch if the lookup is fast). Decide the coherence strategy explicitly.
- **Budget/preemption** with predicted transfers: the inner loop must keep ticking
  the budget. Does a predicted transfer stay in the compiled-code inner loop (like
  `RET_CHAIN`) or return to the dispatcher? Preserve `Blocks(n)` exactness.
- **AOT / persistent cache** (if in scope): key (guest bytes hash → serialized
  code)? Cranelift can emit position-independent code + relocations; persisting and
  re-linking is the hard part. Cross-run cache invalidation. Decide in/out of scope
  and sequence it.
- **Measurement**: a call/return-heavy benchmark (a recursive or deeply-nested-call
  workload) to show the win, plus confirmation that call/ret-light workloads don't
  regress. The existing whole-program suite (busybox, sqlite, lua, CPython — all
  call-heavy) is the real test; add a focused microbench.

## Deliverable requested

A phased plan (e.g. R1, R2, …) with concrete, independently-landable, independently
-testable tasks, risk-ordered, each stating: what it changes (files/functions),
how it preserves each invariant (esp. SMC coherence for cached code pointers,
`Blocks(n)` preemption, and the wrong-prediction slow path), which tests validate
it, and the expected perf effect. Call out ABI changes and the SMC-coherence
strategy explicitly. Recommend the smallest correct first task to implement now,
with its acceptance test. Decide whether AOT/persistent caching belongs in this
track or is deferred.
