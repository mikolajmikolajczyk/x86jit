---
id: decision-3
title: 'JIT reads demand-zero for an unmapped-in-span access; interp traps — known oracle gap'
date: '2026-07-06 11:22'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

## Context

A guest access to an address that is **inside the flat span** (`< size()`) but
**outside every mapped region** is handled differently by the two backends:

- **Interpreter** — `Memory::read`/`write` go through `region_at`, which requires the
  range to fall inside a mapped `Region`; otherwise `MemTrap::Unmapped` →
  `Exit::UnmappedMemory`.
- **JIT** — `codegen::checked_addr` bounds the address only against `MemCtx.size`
  (the flat span), with no region-membership check. An in-span-but-unmapped address
  passes and dereferences `base + addr`, reading the backing (demand-zero for a fresh
  `Vec`/`MAP_NORESERVE` page) and running on.

So a wild/nil guest pointer that lands in-span faults under the interpreter but
silently reads `0` (and, for `Reserved`, silently commits a NORESERVE page) under the
JIT. This breaks the project's core `interp == JIT` invariant.

**Why it is bounded.** Only an incorrect guest reaches it — a correct program keeps
every access inside a region it mapped. The whole differential corpus
(busybox/alpine/glibc/sqlite/lua/cpython + native oracle) never exercises it, which
is why all three-way tests stay green.

## Decision

**Accept the gap for now; do not bandaid it.** Record it here and pin the current
behavior with a differential test
(`x86jit-tests::jit::unmapped_in_span_access_diverges_interp_vs_jit_known_gap`) so it
can't drift silently. The correct semantics is the interpreter's (a nil-deref should
fault); the JIT is the permissive side.

## Alternatives considered

- **Per-access region check in the JIT hot path** — rejected. It rewrites the single
  inlined `base + addr` translation that ADR-0001 deliberately keeps flat, adding a
  table walk to *every* RAM access on *every* guest — a permanent slowdown to catch
  only buggy guests. This is the "special case on shared infra" anti-pattern.
- **Relax the interpreter to match the JIT** (drop the region trap for Flat/Reserved)
  — wrong direction: it makes the correct backend match the buggy one, and throws
  away real fault detection the interpreter already provides for free.
- **Guard pages** — `mmap`/`mprotect` the unmapped holes as `PROT_NONE` and let the
  hardware fault land in a `SIGSEGV` handler. Zero hot-path cost, and how production
  JITs do it. This is the intended resolution, but it needs the signal-delivery
  infrastructure that is deferred to **Phase 3** (`go-caddy-plan.md`) — not landable
  standalone today.

## Trigger to revisit

Phase-3 signals landing (a `SIGSEGV`/guard-page path), or any guest that depends on a
real fault from an in-span-unmapped access. When guard pages arrive, the JIT arm of
the pinning test becomes `UnmappedMemory` too — update the test and close this entry.

## Links

- `x86jit-cranelift/src/codegen.rs` (`checked_addr` — the flat bound).
- `x86jit-core/src/memory.rs` (`region_at` — the interpreter's region trap).
- ADR-0001 (`backlog/decisions/0001-reserved-sparse-mmap-backing.md`) — the flat one-add
  translation and the "no per-page protection" stance this extends.
- the `code-review` milestone task #4 — the finding this resolves.
