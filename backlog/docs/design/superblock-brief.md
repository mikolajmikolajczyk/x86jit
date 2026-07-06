---
id: doc-14
title: 'Superblocks (M5-T3) — planning brief'
type: specification
created_date: '2026-07-06 11:25'
---

# Superblocks (M5-T3) — planning brief

Input for designing the superblock/trace JIT work. Profiling (below) justifies it
per spec §12 M5 ("superblocks / traces, if worth it").

## Goal

Cut the JIT's per-guest-block-boundary overhead. Today each guest basic block is
lifted and compiled as a **separate Cranelift function**; guest registers
round-trip through `CpuState` memory at every block boundary, and control returns
to the dispatcher between blocks (chained via link slots). For code with small
blocks and hot loops (e.g. SHA-256), this boundary cost dominates.

Target: compile a **region of guest blocks** (a trace with internal control flow,
including loops) as **one** Cranelift function, carrying guest registers as SSA
values (Cranelift block params) across the region — flushing to `CpuState` only at
region exits and traps — so a hot guest loop becomes a real host loop with its
working set in host registers.

## Profiling (SHA-256, release, 5000 iters; ablation — no `perf` available)

Baseline JIT: **18.1 ms** (interp 210 ms; native 2 ms → JIT ≈ 9× native, 12× interp).

| Ablation | Time | Attributed cost |
|----------|------|-----------------|
| bounds-check removed (`checked_addr` = base+addr) | 16.4 ms | **~9%** — the per-access `cmp`+`brif`+fault block |
| cross-block dead PF/AF dropped at block boundary | 17.7 ms | **~2%** — parity `popcnt`/AF nibble are cheap on x86 |
| baseline | 18.1 ms | — |

**Conclusion:** flags are already well-optimized (compile-time dead-flag elimination
shipped). Bounds-checks are ~9% (→ guard pages, separate work). The remaining
**~85%** is per-block-boundary overhead: **register round-trips through `CpuState`
+ dispatch**. Confirmed by disassembly — hot blocks are 5–36 guest bytes and every
boundary reloads/stores GPRs from/to `CpuState`. This is what superblocks-with-SSA
attack. A *memory-register* superblock (real CFG but regs still in memory) would
**not** move this — the round-trip stays per iteration; SSA loop-carried registers
are the point.

## Current architecture (what a superblock must integrate with)

- **Lift** (`x86jit-core/src/lift.rs`): `lift_block(mem, start) -> IrBlock`. Decodes
  one guest **basic block** — straight-line until the first control-flow insn, which
  becomes the terminator. `IrBlock { guest_start, ops: Vec<IrOp>, temp_count,
  guest_len, icount }`. A post-lift pass `elide_dead_flags` narrows ALU `set_flags`
  masks by intra-block liveness (all flags conservatively live at block end).
- **IR terminators** (`IrOp`): `Jump{target}`, `Branch{cond, taken, fallthrough}`,
  `Call{target, return_addr}`, `Ret`, `Syscall`, `Hlt`. Targets are `Val::Imm(addr)`
  (static) or `Val::Temp` (indirect/dynamic).
- **Codegen** (`x86jit-cranelift/src/codegen.rs`): `translate_block(builder, ir,
  offsets, alloc_slot, helpers, consistency)` builds one Cranelift function.
  Terminators: `Jump{Imm}`/`Branch` store RIP then **chain via a link slot**
  (`chain_or_link`): return `RET_CHAIN` (slot filled → dispatcher jumps to
  `MemCtx.next_entry`) or `RET_LINK` (cold → dispatcher fills the slot). Indirect
  jump / call / ret store RIP and return `RET_CONTINUE`. Registers: `read_gpr`/
  `write_gpr` load/store `CpuState`; a per-block **write-through GPR value cache**
  (`gpr_cache`) memoizes within one block, invalidated after cpuid/x87/string
  helpers. Memory access: `checked_addr` emits a bounds check → fault block returns
  `RET_UNMAPPED` with fault fields in `MemCtx`; else `base + addr`.
- **ABI** (`x86jit-core/src/jit_abi.rs`): compiled fn is `extern "C" fn(cpu: *mut u8,
  mem: *mut u8) -> u64`. `MemCtx { base, size, fault_addr, fault_size, fault_access,
  next_entry, link_slot }` `#[repr(C)]`. Return codes: `RET_CONTINUE=0`,
  `RET_SYSCALL=1`, `RET_HLT=2`, `RET_UNMAPPED=3`, `RET_CHAIN=4`, `RET_LINK=5`,
  `RET_EXCEPTION=6`. There is **no budget field** in the ABI today.
- **Dispatcher** (`x86jit-core/src/vm.rs::Vm::run`): `budget: Option<u64>` counted in
  **blocks**; `blocks_run += 1` per compiled-block call; the inner chain loop keeps
  jumping on `RET_CHAIN` and re-checks budget each hop, so a tight chained loop still
  yields `Exit::BudgetExhausted` (preemption §9.2). `handle_smc()` runs before each
  resolve.
- **Cache** (`x86jit-core/src/cache.rs`): keyed by guest start `pc`; stores the
  compiled entry + `guest_len` (one contiguous span). `mark_code(start, len)` tags
  the block's pages so a store onto them invalidates it (SMC, §10 / M6).
- **Backend trait**: `materialize(&self, ir: &IrBlock, consistency) -> CachedBlock`.
  `Vm` is shared across vcpus behind `Arc` (M7); the cache and guest RAM are shared;
  `CpuState` is per-vcpu.

## Hard invariants a superblock must not break

1. **interp == JIT == Unicorn** — the differential/fuzz/corpus oracles compare full
   CPU state (regs + flags) at `hlt`. Register/flag values at every trap-out and at
   block end must match the interpreter exactly.
2. **RIP-retry / instruction atomicity** (spec pitfall #0, §16): on a mid-block trap
   (unmapped/MMIO/#DE/syscall) the guest state must be consistent with "instructions
   up to the faulting one committed; the faulting one not." With SSA registers held
   in host regs, **every trap-out inside the region must flush the live guest regs to
   `CpuState` first**, and set RIP to the right guest address.
3. **Preemption (§9.2)**: a tight guest loop must still yield `BudgetExhausted`. An
   internal host loop returns to the dispatcher rarely, so the budget must be
   decremented/checked *inside* the region (needs an ABI budget field or equivalent).
4. **SMC (§10 / M6)**: a store onto any byte of any block in the region must
   invalidate the whole superblock. `mark_code` takes one contiguous `(start, len)`.
   A region may be non-contiguous; the invalidation must cover all its bytes.
5. **M7 threading**: `Vm`/cache shared behind `Arc`; link slots are written
   single-threaded today (atomics deferred). SMC invalidation and cache insertion
   must stay sound under concurrent vcpus.

## Design space / open questions for the plan

- **Region formation**: DFS from the entry over static (`Imm`) `Jump`/`Branch`
  targets; stop at indirect/`Call`/`Ret`/`Syscall`/`Hlt`, at a size cap (blocks or
  total insns), and treat a target already in the region as an internal edge
  (back-edge → loop). What caps? How to represent the region IR (a list of sub-blocks
  keyed by guest addr + their edges)?
- **SSA register state**: carry which registers as block params — all 16 GPRs (+
  RSP), and flags? Live-in at region entry, phi at merges, flush at every exit/trap.
  How to keep this correct and let Cranelift's regalloc prune unused ones.
- **In-region budget/preemption**: add a budget counter to `MemCtx` (ABI change) and
  decrement at back-edges, returning to the dispatcher when exhausted (RIP at the
  loop header). Or another mechanism.
- **SMC over a region**: `mark_code` per sub-block, or a superblock-aware
  invalidation covering `[min, max]` (over-approx, safe). Cache keying: still by
  entry `pc`, but the span must cover the whole region.
- **Traps inside the region**: flush live SSA regs + set RIP before returning
  `RET_UNMAPPED`/`RET_EXCEPTION`/`RET_SYSCALL`. Helpers (div/x87/string/cpuid) that
  read/write `CpuState` need the current values flushed before the call and reloaded
  after.
- **Interaction with existing block chaining**: a region exit to an out-of-region
  static target can still chain via a link slot. Indirect/ret exits go to dispatch.
- **Incremental delivery**: what is the smallest correct, testable first step, and
  the risk-ordered phases after it? (e.g. self-loops only first? straight-line region
  merge first without loops?)
- **Testability**: every phase must keep the full suite green (differential, fuzz,
  corpus vs Unicorn; the real programs; SMC and threading tests) and ideally show a
  measurable SHA-256 improvement.

## Deliverable requested

A phased plan (M5-T3a, T3b, …) with concrete, independently-testable tasks,
risk-ordered, each stating: what it changes, how correctness is preserved (which
invariants, which tests), and the expected perf effect. Call out the ABI/SMC/
preemption changes explicitly and where they land in the sequence.
