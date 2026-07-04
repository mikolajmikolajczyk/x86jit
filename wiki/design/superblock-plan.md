# M5-T3 Superblocks — Implementation Plan

**Status:** T3a ✅ (fuel ABI). T3b ✅ (straight-line regions, multi-span
cache/SMC, backend gating). T3c ✅ (DAG regions: `lift_region` DFS over branch
arms → reverse-post-order; `translate_region` real Cranelift CFG with internal
`brif`/`jump` for forward/merge edges, chain exits for back-edges; diamond +
600-program fuzz + real-program validation). Registers still write-through.
T3d ✅ (back-edges internalized → real host loops; fuel gate keeps them
preemptible; loop test + real-program validation). Registers still write-through
(~neutral perf so far, as predicted). T3e ✅ (SSA loop-carried GPRs + fuel as
Cranelift Variables; flush discipline via `ret`, helper flush/reload,
`ret_no_flush` on helper traps; 600-fuzz + whole real-program suite validated
with regions on). **Execution 18.1 → 6.3 ms warm (~3× faster, ~3× native)** — but
the region *compile* is heavier, so a short run regresses. T3f ✅ (loop-only
formation: `IrRegion::has_loop` gates region compile to loops — only they iterate
enough to amortize it; loop-free code stays single-block). **Decision: superblocks
stay OPT-IN, not default-on.** Measured: even loop-only, default-on regresses
python 90 s → 280 s — the region compile cost is real and workload-dependent, so
it's a per-workload knob (`JitBackend::with_superblocks(caps)`, like the
MemConsistency tiers), giving ~3× execution on hot-loop workloads at higher
compile cost. **Future path to default-viability:** hotness-gated tier-up (compile
a region only after a loop is proven hot by an execution counter) + written-set
flush + a lower region opt-level. M5-T3 complete as an opt-in capability.

Authored by Fable 5 (Plan agent) from [`superblock-brief.md`](superblock-brief.md),
grounded in the code. Load-bearing facts independently verified: the differential
oracle runs `RunSpec::Blocks(n)` (`x86jit-tests/src/vector.rs`, `oracle.rs`) so it
compares full state at block-budget exhaustion — exact in-region block accounting is
a **correctness** requirement, not a nicety; and `MemCtx` is 7×u64, so a new `fuel`
field lands at offset **56**.

## Design decisions (resolved up front)

**D1 — Region model.** A superblock is `IrRegion { entry: u64, blocks: Vec<IrBlock> }`
formed by worklist DFS from the entry over static targets (`Jump{Val::Imm}`, both
arms of `Branch`). `Call`/`Ret`/`Syscall`/`Hlt`/indirect `Jump{Val::Temp}` end
exploration (region exits). A target equal to an in-region block's `guest_start` is an
internal edge; a back-edge is an internal edge to an already-visited block. Caps:
`max_blocks = 16`, `max_icount = 256` initially (tuned in T3f). A lift error on a
successor truncates that edge into an exit; the region stays valid. Cache keying stays
by entry `pc`; a jump into the middle of a region misses and compiles its own
block/region (trace duplication — accepted).

**D2 — Register carry.** All 16 GPRs (incl. RSP) become Cranelift **`Variable`s**
(`declare_var`/`def_var`/`use_var`), replacing `gpr_cache` inside regions — Cranelift's
SSA builder inserts phi/block-params and prunes dead ones for free, instead of
hand-managing 16 block params per guest block. The region former computes a static
**read-set** (loaded from `CpuState` once at entry) and **written-set** (flushed at
every exit and trap-out). RIP, `fs_base`/`gs_base`, flags, and XMM stay
memory-resident.

**D3 — Flags.** Stay write-through `CpuState` stores as today; `elide_dead_flags` stays
per sub-block (conservative all-live at each sub-block boundary). Rationale: flags cost
~2% (ablation), and fuel can exit the region at *any* internal edge, so flags must be
architecturally correct at every edge anyway. Cross-block flag elision is out of scope.

**D4 — Budget/preemption (the ABI change).** Add `pub fuel: u64` to `MemCtx`
(`MEMCTX_FUEL = 56`). The dispatcher writes the remaining block quantum before each
call; a compiled *region* consumes 1 fuel per guest block executed beyond the first
(decrement + zero-check at **every internal edge**, held as an SSA value in-region,
flushed at exits); single blocks never touch it. On exhaustion the region flushes
carried registers, sets RIP = the edge-target block's start, and returns plain
`RET_CONTINUE` — **no new RET code**. Dispatcher accounting:
`blocks_run += max(1, quantum - ctx.fuel)` — bit-identical to `+= 1` for single blocks,
and exactly the interpreter's per-block count for regions, preserving `RunSpec::Blocks(n)`
equivalence and §9.2 preemption. Tradeoff: one reg dec+brif per guest block (vs
back-edge-only), but back-edge-only would break the `Blocks(n)` oracle; cost negligible
vs the round-trips replaced.

**D5 — SMC over a non-contiguous region.** `TranslationCache::spans`:
`HashMap<u64, u32>` → `HashMap<u64, Vec<(u64, u32)>>` (`insert` takes a span list;
single blocks pass one). `resolve()` calls `mark_code(start, len)` per sub-block span;
`invalidate_overlapping` tests every span — a store on any byte of any sub-block kills
the whole region by its entry key. Rejected: `[min,max]` hull — would tag unrelated
pages between disjoint spans → spurious invalidations.

**D6 — Backend gating (no `VmConfig` churn).** `trait Backend` gains
`fn region_caps(&self) -> Option<RegionCaps> { None }` and
`fn materialize_region(&self, region, consistency) -> CachedBlock` (only called when
caps are `Some`). `InterpreterBackend` keeps the default `None` → interpreter, mixed
configs, and every `VmConfig` literal untouched. `JitBackend::with_superblocks(caps)`
opts in; `new()` flips the default only in T3f. `resolve()` consults `region_caps()`
and falls back to the single-block path when region formation yields one block.

**D7 — Traps and helpers inside a region (RIP-retry).** Every trap-out flushes the
written-set: `checked_addr`'s fault block (before `RET_UNMAPPED`), `emit_div`'s `#DE`
path (before `RET_EXCEPTION`), `Syscall`/`Hlt` terminators, and fuel exhaustion.
Helpers that mutate `CpuState` (`string_helper`, `x87_helper`, `cpuid_helper`) get
flush-before-call / `def_var`-reload-after-call; their fault paths return immediately
after the call **without** re-flush (the helper's `CpuState` writes are authoritative —
flushing stale SSA after would corrupt e.g. a partial `rep movs`). `div_helper` writes
only out-params, so only its exception path flushes. Flags/RIP/XMM are memory-resident,
so "flush" = the GPR written-set only; RIP-retry holds because RIP is stored with
`cur_addr` at every trap site as today. Each region emits one shared flush-and-return
block that all `checked_addr` fault sites jump to (bounds code bloat).

**D8 — Threading (M7).** Nothing new: region compilation under the existing
`JitBackend` `Mutex`, cache insertion under the `RwLock`, `mark_code` atomic bits,
`fuel` in the per-vcpu stack `MemCtx`. Region exits reuse `chain_or_link` slots with the
same single-threaded-write deferral.

## Phases (risk-ordered, each independently landable + testable)

### M5-T3a — Fuel ABI + dispatcher plumbing (behavior-neutral)
Add `fuel: u64` to `MemCtx` (`MEMCTX_FUEL = 56`, init `u64::MAX` in `for_memory`). In
`Vcpu::run`, before each `call_block` (initial + every `RET_CHAIN`/`RET_LINK` hop) set
`ctx.fuel = budget.map_or(u64::MAX, |b| b - blocks_run)`; after,
`blocks_run += max(1, quantum - ctx.fuel)` replacing `+= 1`. No codegen change →
`quantum - ctx.fuel == 0` always → bit-identical behavior. Lands the highest-coordination
change first, inert. **Tests:** whole suite unchanged (esp. `jit.rs::chained_loop_still_yields_budget`,
`mt.rs`, `tso.rs`, all `differential.rs` `Blocks(n)`); new unit test asserting the offset
and that a plain block leaves `fuel` untouched. **Perf:** none.

### M5-T3b — Region infra + straight-line superblocks (opt-in)
`IrRegion`/`RegionCaps`/`lift_region` following only `Jump{Val::Imm}` edges
(straight-line concat; `Branch` arms are exits); read/written sets + span list.
`Backend::region_caps`/`materialize_region`; region-aware `resolve()` + multi-span
`mark_code`. Multi-span `spans` map + `regions` stat. `translate_region`: sequential
sub-blocks in one function, drop internal `Jump` (fallthrough), reset temps + clear
`gpr_cache` per sub-block (still write-through/memory-resident — `CpuState` always
current, no flush logic yet); fuel decrement/check per internal edge.
`JitBackend::with_superblocks(caps)`. **Tests:** suite green with regions off; flag-on
jump-chain differential incl. `Blocks(1)` stopping at the internal boundary; SMC test
writing into the second sub-block's bytes; env-gated (`X86JIT_SUPERBLOCKS=1`) full
differential/fuzz sweep. **Perf:** ~neutral (retires plumbing risk).

### M5-T3c — DAG regions (internal conditional control flow)
`lift_region` admits `Branch` arms + `Jump{Imm}` to visited blocks when no cycle (DAG).
`translate_region` grows real CFG: pre-create one Cranelift `Block` per sub-block,
`addr→Block` map; `Branch`/`Jump{Imm}` → `brif`/`jump` to internal blocks when in-region,
else existing exit; correct seal order. Fuel check on every internal edge; `gpr_cache`
cleared per sub-block (safe: write-through). **Tests:** flag-on diamond/if-else
differential + fuzz; `Blocks(n)` stopping mid-diamond each arm; SMC over non-contiguous
sub-blocks. **Perf:** small (removes in-region dispatcher hops; register round-trip
remains).

### M5-T3d — Back-edges: loops, preemption proven
Drop the DAG restriction; handle loop-header sealing (deferred `seal_all_blocks`). A
guest loop becomes a host loop that provably yields via fuel. **Tests (headline
preemption §9.2):** `chained_loop_still_yields_budget` with superblocks on (self-loop
region, budget 1000 → `BudgetExhausted`); interp-vs-JIT `Blocks(n)` mid-loop with
identical `rcx`/flags; `mt.rs`/`tso.rs` superblocks-on; full flag-on sweep. **Perf:**
~neutral on SHA-256 (the "memory-register superblock" the brief predicts won't move the
needle) — measure to isolate dispatch-elimination from register-carry.

### M5-T3e — SSA loop-carried registers (the payoff)
Region mode replaces `gpr_cache` with Variables (D2): entry `def_var`s the read-set;
`read_gpr`→`use_var`, `write_gpr`→merge then `def_var` **without** `store_cpu`. Flush
discipline per D7 at every exit/trap/helper. Fuel becomes a carried Variable.
Single-block path untouched. **Tests (correctness-critical):** full
differential+fuzz+corpus vs Unicorn (fuzz mid-block faults = RIP-retry oracle); new:
unmapped store in the 2nd loop iteration, `rep movs` fault in-region, `#DE` in-region,
syscall in-region with dirty carried regs, `Blocks(n)` mid-loop; whole-program suite
superblocks-on. **Perf:** the target — plausibly 18.1 ms → 5–8 ms (9× → ~3–4× native).

### M5-T3f — Default-on, formation policy, caps, stats
Only keep a region with a back-edge or ≥2 blocks (else single-block+chaining). Tune
caps. Flip `JitBackend::new()` on. Update chaining-assertion tests to the `regions`
counter (keep a chaining variant with regions off). Out-of-scope follow-ups: XMM as
carried Variables, hotness-gated formation, region-level flag liveness, guard-page
bounds-check elimination (the separate ~9%).

## Where cross-cutting changes land

| Change | Phase | Why |
|---|---|---|
| ABI (`MemCtx.fuel`, accounting) | **T3a** | Highest-coordination; inert alone; must precede any region so `Blocks(n)` equivalence never breaks. |
| Backend trait (`region_caps`) | T3b | First backend divergence; default-`None` keeps interp/mixed untouched. |
| SMC/cache (multi-span) | **T3b** | Needed the instant a unit spans >1 contiguous range; isolate with simplest regions. |
| Preemption (in-region fuel) | T3b mechanism / **T3d** proven | Exact from first multi-block region; back-edges admitted once it yields. |
| SSA flush discipline | **T3e** | Deepest correctness change last, after every region shape + fuel + multi-span SMC soaked. |

## First task: M5-T3a
Add `fuel: u64` to `MemCtx` (offset 56, `u64::MAX` default) and switch `Vcpu::run` to
quantum-in / `blocks_run += max(1, quantum - ctx.fuel)` around every `call_block`
including the chain loop. ~30 lines, no codegen change, no observable behavior change.
**Acceptance:** full suite passes unmodified (esp. `chained_loop_still_yields_budget`,
all `Blocks(n)` vectors, `mt.rs`/`tso.rs`); new unit test asserting `MEMCTX_FUEL == 56`
and that a compiled block leaves `ctx.fuel` at its init value (consumed == 0).
