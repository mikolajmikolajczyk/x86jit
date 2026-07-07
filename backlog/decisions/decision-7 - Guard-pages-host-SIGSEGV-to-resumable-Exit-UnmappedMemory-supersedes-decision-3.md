---
id: decision-7
title: >-
  Guard pages: host SIGSEGV to resumable Exit::UnmappedMemory supersedes
  decision-3
date: '2026-07-07 12:07'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

> **Supersedes [[decision-3]]** (2026-07-07). decision-3 accepted, as a bounded
> interim, that the JIT reads demand-zero for an in-span-but-unmapped access while
> the interpreter traps — a known `interp == JIT` oracle gap. Guard pages close it
> for host-backed spans: the gap now survives **only** on the `Vec`-backed `Flat`
> path (no host pages to protect), which **GP-5** (task-152) removes by host-backing
> that path too.

## Context

A guest access inside the flat span but outside every mapped region diverged: the
interpreter's `region_at` traps `Exit::UnmappedMemory`, while the JIT's `checked_addr`
bounds only against the flat span (ADR-0001's single inlined `base + addr`, no
per-access region walk) and reads demand-zero. decision-3 accepted this rather than
add a table walk to every RAM access on every guest — the "special case on shared
infra" anti-pattern.

decision-3 named guard pages as the intended resolution but deferred them to Phase-3
signals. That coupling turned out to be unnecessary: making the fault **visible** as a
resumable `Exit::UnmappedMemory` needs only host memory protection + a SIGSEGV handler
that `siglongjmp`s back to the dispatcher — not guest signal *delivery* (still
task-123). This is doc-30 (`backlog/docs/design/guard-pages-sigsegv.md`).

## Decision

**Host-back the unmapped holes with `PROT_NONE` guard pages and recover the hardware
fault into a resumable `Exit::UnmappedMemory`.** Zero hot-path cost — codegen is
unchanged; the flat `base + addr` translation stays. Delivered doc-30 GP-1..GP-3:

- **GP-1** — `Memory::map`/`unmap` drive an embedder `protect` callback that
  `mprotect`s a region RW on map, `PROT_NONE` on unmap (page-granular, respecting
  neighbours). `hostmem::reserve_guarded` maps the whole span `PROT_NONE` up front.
- **GP-2** — a process SIGSEGV handler classifies the fault: address inside an armed
  guest span → `siglongjmp` to `guarded_run`, which returns
  `Exit::UnmappedMemory { addr, access }` (access from hardware, D4). Any other fault
  (no armed guard, or an address outside every span — a genuine host bug, incl. the
  JIT arena's W^X) restores the previous disposition and re-fires, so the process
  still crashes honestly with its core dump.
- **GP-3** — a `set_srcloc(guest_rip)` side table (`x86jit-core::codemap`, append-only,
  async-signal-safe) maps the faulting host PC back to the precise guest RIP, so the
  recovered `Exit` is resumable on the faulting instruction — identical to the
  interpreter, single-block and region.

The correct semantics is the interpreter's; the JIT now matches it for every
host-backed span. Precision recorded: `addr`/`access`/`RIP` exact; GPRs exact in
single-block mode, may be stale in region mode (full-register precision at an async
fault needs deopt metadata — out until task-123 builds a guest signal frame).

## Consequences

- The positive behaviour (both backends fault) is pinned in
  `x86jit-tests/tests/guard_pages.rs` (in-span load/store/nil-deref → `UnmappedMemory`,
  precise-RIP parity, region-mode RIP, GPR fault-before-commit, plus a subprocess
  honesty test that a non-guest SIGSEGV still crashes).
- The **residual** gap — `Vec`-backed `Flat` still reads demand-zero under the JIT —
  is pinned by `unmapped_in_span_vec_backed_residual_gap` in `jit.rs` until GP-5
  host-backs the Flat path.
- **glibc host assumption**: `guarded_run` binds glibc's `__sigsetjmp` (the C macro).
  The x86jit host toolchain is glibc (nix devShell + CI); a musl host would need a
  small C shim. The guest may be musl — unrelated (this is host-side).
- Guest signal *delivery* (turning the `Exit` into a Go nil-panic) stays task-123;
  guard pages only make the fault visible.

## Links

- `backlog/docs/design/guard-pages-sigsegv.md` (doc-30) — full design + phases.
- `x86jit-core/src/codemap.rs`, `x86jit-linux/src/sigsegv.rs`,
  `x86jit-linux/src/hostmem.rs` (`reserve_guarded`), `x86jit-core/src/memory.rs`
  (`protect`/`reprotect`).
- [[decision-3]] (superseded) · task-127 (umbrella) · tasks 148–152 (GP-1..5).
