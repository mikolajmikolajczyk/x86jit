---
id: doc-30
title: 'Guard pages — host SIGSEGV → resumable Exit::UnmappedMemory (closes decision-3)'
type: specification
created_date: '2026-07-07'
---

# Guard pages: host SIGSEGV → `Exit::UnmappedMemory` under the JIT (task-127)

Ready-to-execute plan for task-127: make an **in-span-but-unmapped** guest access fault
under the JIT exactly as it already traps under the interpreter, by making the unmapped
holes of a host-backed span `PROT_NONE` and converting the resulting hardware SIGSEGV
into a resumable `Exit::UnmappedMemory`. Closes decision-3 (whose "guard pages"
alternative this is). Authored by Fable 5 (architect session, 2026-07-07); grounded in
the working tree at `83bbe60`.

**Maintainer-ratified open decisions (2026-07-07):** D1 full-closure by inversion;
`CodeMap` lives in `x86jit-core`; phase order is feature-first (GP-2 before GP-3);
GP-5 (host-back the Flat path) is in scope.

## The gap (decision-3, verified)

`codegen::checked_addr` (x86jit-cranelift/src/codegen.rs ~2090) bounds a guest access
only against `MemCtx.size` (the flat span), no region-membership check: an
in-span-but-unmapped address dereferences `base + addr` and reads demand-zero, where the
interpreter's `region_at` (memory.rs ~579) traps `MemTrap::Unmapped` →
`Exit::UnmappedMemory`. Under `Reserved { span: 1 TiB }` (Go), page 0 is in-span, so a Go
nil-deref silently reads zero under JIT — breaking interp==JIT and Go's nil-panic
semantics. Decision-3 accepted this and named guard pages as the fix, gated on P3 signals
(now landed).

## The model (settled)

### D1 — Guard strategy: full-closure by inversion (host-backed memories only)

- `hostmem::reserve` mmaps the span **`PROT_NONE`** (was RW), `MAP_NORESERVE`.
- `HostRam` gains an embedder-injected `protect: Option<Box<dyn Fn(*mut u8, usize, bool)
  + Send + Sync>>` — same pattern as the existing `dtor`. The `mprotect` lives in
  x86jit-linux; **core stays `{iced-x86}`**.
- `Memory::map` invokes it for the region's page range (rounded **outward**) →
  `PROT_READ|PROT_WRITE`. `Memory::unmap` invokes it (rounded **inward**, with an
  edge-page check against remaining `regions` so a page shared with a live neighbor stays
  RW). Core owns the rounding (it has the region table); the callback only flips protection.

Cost: the region table is **static after load** (regions created at load; guest `mmap` is
a bump allocator inside pre-mapped arenas, `munmap`/`mprotect` are shim no-ops), so ~5-7
`mprotect` at setup, **zero at runtime** — same cost as nil-page-only, but full closure
(nil page, the #14 stack guard band becomes hardware-enforced, brk↔mmap hole).

Residual gaps (recorded in decision-7): sub-page region edges stay accessible on a shared
page (Go's holes are page-aligned — moot); **Vec-backed** memories (`Memory::new` boxed
backings — test VMs; x86jit-run's non-Go Flat until GP-5) can't be `mprotect`ed → no
guards.

### D2 — Signal handler + published state

New `x86jit-linux/src/sigsegv.rs`. Installed once (`Once`, `SA_SIGINFO`, save old
disposition for chaining). Per-host-thread `thread_local! UnsafeCell<GuardSlot>`:

```
GuardSlot { active: bool, jmp: sigjmp_buf, mem_base: u64, mem_size: u64,
            cpu: *mut CpuState, fault_addr: u64, fault_access: u8 }
```

`guarded_run(&mut Vcpu, &Vm, budget) -> Exit` (the wrapper `thread.rs`/`proc.rs` call
instead of `cpu.run`) publishes base/size/cpu **once per run() slice** (not per block —
zero hot-path cost), `sigsetjmp(jmp, 1)`, sets `active`, runs, clears `active`.

Handler (async-signal-safe — only TLS + `siglongjmp`):
1. `!active` → not a guest context → **chain/re-raise** (honest crash, core dump intact).
2. `si_addr ∉ [mem_base, mem_base+mem_size)` → genuine host bug (incl. JIT-arena W^X,
   whose si_addr is in the arena) → chain/re-raise **even while active**.
3. Interrupted PC ∉ any registered JIT code range → host-code fault in the span → re-raise.
4. else guest fault: `guest_addr = si_addr - mem_base`; access kind from the arch seam;
   write fault_addr/access + recovered guest RIP into the slot; `siglongjmp(jmp, 1)`.

The interpreter never reaches the handler (`region_at` pre-traps; host accessors check
`region_for`). No `sigaltstack` in v1 (a guest stack overflow faults into the guarded
guest-span band at ordinary host depth — handler runs; a host stack overflow force-kills,
which is the honest crash we want).

### D3 — Recovery: sigsetjmp in the embedder wrapper + siglongjmp from the handler

`sigsetjmp(env, savesigs=1)` once per `run()` slice in `guarded_run` (x86jit-linux, so
`sigsetjmp`'s libc stays out of core). `siglongjmp` from the handler. Zero per-block cost.
`savesigs=1` restores the pre-handler mask (SIGSEGV blocked in-handler; else the next
fault force-kills).

**Soundness of jumping over `Vcpu::run` frames**: a guard fault fires only inside compiled
code; skipped frames are JIT blocks (no dtors) + run()'s loop, whose live locals are POD
(`MemCtx`, counters, `CompiledPtr`, `CachedBlock::Compiled{entry}` — a bare ptr, no Arc).
**Invariant (comment tripwire at vm.rs ~676): no Drop-owning local live across
`call_block`.**

After longjmp, `guarded_run` returns `Exit::UnmappedMemory { addr, access }` — same shape
as `RET_UNMAPPED`, resumable (RIP on the faulting insn).

**Faulting guest RIP — srcloc side table (GP-3, not a per-access spill):**
- codegen `builder.set_srcloc(SourceLoc::new(guest_rip as u32))` at `IrOp::InsnStart`
  (guest code executes below the 4 GiB CODE_WINDOW → u32 safe). Zero emitted instructions.
- at compile, capture `ctx.compiled_code()` code size + `get_srclocs_sorted()` → boxed
  `[(host_off: u32, guest_rip: u32)]`; register `(entry, code_len, table)`.
- **`CodeMap`** = new pure-data module in **x86jit-core** (no OS deps): process-global,
  **append-only** chunked storage (chunks never move) + release/acquire `AtomicUsize`
  length → async-signal-safe to read. Append-only is correct (cranelift-jit never frees
  code during the module's life; an SMC-dropped block's code bytes are unchanged). Handler:
  host PC → containing range → largest `host_off ≤ pc-entry` → guest RIP → `cpu.rip`.

Access kind comes from **hardware** (a single insn can read+write, e.g. `movs`), via D4.

**Precision (recorded):** addr/access/RIP exact. GPRs exact in single-block mode (eager
`store_cpu`), **may be stale in region mode** (Variable-carried GPRs). Full register
precision at an async fault needs deopt metadata — OUT (only matters when task-123 builds
a guest SIGSEGV frame from a JIT fault).

### D4 — Platform seam (x86-64 + aarch64)

`mod mcontext` in sigsegv.rs, two `#[cfg(target_arch)]` impls (~40 lines):
- `fault_pc(uc)` — x86-64 `gregs[REG_RIP]`; aarch64 `uc_mcontext.pc`.
- `is_write(uc, si) -> Option<bool>` — x86-64 `gregs[REG_ERR] & 0x2`; aarch64 walk
  `__reserved` for `ESR_MAGIC`, `ESR_ELx.WnR` (bit 6). `None` → default `Read`.

No PC *mutation* (D3 longjmps), removing the riskiest per-arch code. W^X arena is
orthogonal (arena faults have si_addr outside the guest span → honest crash).

### D5 — decision-3 flip

New **decision-7** ("Guard pages: host SIGSEGV → resumable Exit::UnmappedMemory; closes
decision-3"). Amend decision-3 `accepted → superseded`. Flip the pinning test
(`x86jit-tests/tests/jit.rs` ~349) → both arms assert `UnmappedMemory{addr:0x2000,
access:Read}` on a host-backed Reserved VM through `guarded_run`; from GP-3 also assert
`cpu.rip`. Add a narrow residual pin for the Vec-backed Flat gap (until GP-5).

### D6 — deliberately OUT

Guest signal *delivery* (turning the Exit into a Go nil-panic) is **task-123** — this task
only makes the fault visible as `Exit::UnmappedMemory` (→ `report_gap`/`fault_teardown` →
`ProcError::Trapped`, restoring interp==JIT). Also out: Windows/macOS, SoftMmu, sub-page
precision, deopt-precise registers.

## Invariants (tests pin)

1. Hot path unchanged: codegen emits **zero** new instructions; no per-access check/spill;
   bench shows no regression.
2. Any in-span-hole access of a host-backed span → `UnmappedMemory{addr,access}`, RIP on
   the faulting insn — identical under interp and JIT.
3. A SIGSEGV outside every registered span, or PC outside registered JIT code, crashes the
   process as without the handler (chain/re-raise; core dump preserved).
4. `map()` opens exactly the region's pages; `unmap()` closes them except pages shared with
   live neighbors.
5. Reserved span stays sparse (RSS test) with PROT_NONE-default + per-region mprotect.
6. No Drop-owning local live across `call_block`.

## Phases (feature-first, each independently landable + testable)

- **GP-1 — protect-callback plumbing (dark).** `HostRam.protect` (default None, all
  ctors unchanged); rounding in `Memory::map`/`unmap` invoking it; `hostmem::reserve_guarded`
  (PROT_NONE + mprotect callback) beside untouched `reserve`. Tests: core rounding units
  (recording callback, incl. shared-edge unmap); RSS sparseness on `reserve_guarded`.
  Nothing user-visible.
- **GP-2 — handler + `guarded_run` (feature lands).** sigsegv.rs (install-once, TLS slot,
  D2 classification, D4 mcontext seam, D3 sigsetjmp/longjmp, hardware access kind); switch
  thread.rs/proc.rs to `guarded_run`; switch x86jit-run's Go path to `reserve_guarded`.
  Flip the pin (addr+access; RIP not yet). Tests: write-access; nil-page; threaded (one
  JIT guest thread nil-derefs while a sibling parks); subprocess honesty test (raise a
  non-guest SIGSEGV → child dies by signal).
- **GP-3 — precise RIP.** srcloc at InsnStart; capture srclocs+size at compile; `CodeMap`
  in core; handler writes `cpu.rip`. Tests: pin asserts RIP parity; region-mode fault RIP
  exact; single-block GPR parity vs interp.
- **GP-4 — decision flip + docs.** decision-7; decision-3 superseded; residual Vec pin;
  go-caddy-plan Phase-3 note; DoD (nextest green minus fuzz, clippy, fmt).
- **GP-5 — host-back the Flat path.** x86jit-run non-Go Flat via `reserve_guarded` → every
  shim-run guest faults on wild in-span pointers.

## Risks

- **R1 longjmp over Rust frames** — bounded by invariant 6; a future heap-owning dispatcher
  local would leak per (terminal) fault, never UB. Comment tripwire.
- **R2 partial state at fault** — region-mode GPRs stale (documented); fault-before-commit
  ordering matches x86 (lift does loads before reg/flag commits) — verify with a diff state
  test in GP-3.
- **R3 embedder's own SIGSEGV handler** — save-and-chain; conversion is opt-in (only
  `guarded_run` activates it).
- **R4 cranelift srcloc fidelity under opt** — region-mode fault tests; fallback is
  per-instruction granularity within a block (RIP still same-block), flagged by the parity
  test.
- **R5 aarch64 ESR absence** → access kind defaults Read (documented); CI runs both arches.
- **R6 mprotect edges** — round-out on map (over-permissive by design), neighbor-shared
  page on unmap (core check, unit-tested), MAP_NORESERVE preserved (RSS test).
