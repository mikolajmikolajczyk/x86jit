---
id: decision-1
title: '`Reserved` memory model — sparse host-mmap backing, no per-page protection'
date: '2026-07-06 11:22'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

## Context

A real Go runtime (the caddy goal, `go-caddy-plan.md`) reserves a very large,
sparse virtual address space at startup: `mallocinit` `PROT_NONE`-reserves ~600 MiB
of page-summary structures and places heap-arena hints at `0xc000000000` (768 GiB).
The existing `Flat { size }` model eagerly allocates one `vec![0u8; size]`, so it
cannot represent 768 GiB without committing 768 GiB — the guest `mmap` arena topped
out at ~127 MiB and Go aborted in `mpagealloc` with *"failed to reserve page summary
memory"*.

The spec always reserved a `SoftMmu` model slot for "a sparse, high address space,"
left `todo!()`. The naïve reading — a per-access page-table lookup — would rewrite
the hottest path in both backends: every inlined `host_base + guest_addr` RAM access
(`jit_abi.rs`) becomes a table walk, a large codegen change and a permanent slowdown
on **every** guest, not just Go.

## Decision drivers

- Zero cost to the existing corpus (busybox/glibc/musl/lua/python/sqlite three-ways):
  no codegen change, no per-access regression.
- The actual requirement is "sparse, huge, lazily committed", not "per-page
  protection enforcement" — which the engine already does not do (`Prot` is recorded
  but unenforced; guest `mprotect` is a no-op).
- Reversibility: keep the flat one-add translation so a future true SoftMmu remains
  possible without re-touching the hot path now.

## Considered options

1. **True SoftMmu** — per-access page-table lookup, real per-page permissions.
   Correct and general, but rewrites the hottest inlined access in both backends and
   slows every guest. Rejected for now (the requirement doesn't need it).
2. **`Reserved { span }` = flat one-add over a sparse host `MAP_NORESERVE` mmap**
   (chosen). Translation is unchanged; sparseness is entirely at the host-kernel
   level (demand paging). Untouched guest VA costs nothing.
3. Grow `Flat` to a bigger eager `Vec` — or an `alloc_zeroed` `Vec` for `Reserved`.
   Doesn't scale: on a heuristic-overcommit host a plain `calloc` of 512 GiB is
   rejected (no `NORESERVE`), and 768 GiB committed is a non-starter regardless.

**Where the mmap lives — embedder, not core.** `x86jit-core` depends on exactly the
x86 decoder; a boundary tripwire test (`boundary::core_stays_guest_agnostic`) fails
the build if anything else — including `libc` or `memmap2` — is added. A host `mmap`
is an OS facility, so it belongs on the embedder side of the §1/§4.1 line. Core
therefore defines the *model* and an *injection point* (`Memory::from_host_ram`,
`Vm::with_backend_host_ram`) taking a `HostRam { ptr, len, dtor }`; the embedder
(`x86jit-linux::hostmem::reserve`) mints the `MAP_NORESERVE` mapping and hands it in.
The `dtor` `munmap`s on drop. This keeps core `libc`-free.

## Decision outcome

Add `MemoryModel::Reserved { span }`. It reuses the `Flat` code exactly — `map()`
tags and bounds-checks against `span`, `read`/`write` index `host_base + addr`,
`size()` returns `span` (the JIT's bound constant) — so **no backend codegen
changes**. The backing is either a plain `Vec` (via `Memory::new`, for a modest span
or a test) or an embedder-provided `HostRam` (via `Memory::from_host_ram`, the
`MAP_NORESERVE` production path). `Backing` reads through a uniform raw `ptr`/`len`
so the two allocation sources share one access path.

Two accommodations keep a 1 TiB span cheap:

- **SMC code-page table is bounded** to a low `CODE_WINDOW` (4 GiB) instead of one
  `AtomicBool` per 4 KiB page across the whole span (which would itself commit
  hundreds of MiB). Guest code always lives in the low image/interp region and never
  executes from the multi-hundred-GiB heap it reserves, so tracking only the low
  window is correct — a `mark_code`/`note_write` above it simply no-ops.
- **`deep_copy` (fork) copies only tagged regions**, not the whole span (cloning
  1 TiB would commit it). Go never forks; the forking corpus stays on `Flat`.

## Positive consequences

- Go's `mallocinit` can complete once the runner uses `Reserved` (the abort moves
  past page-summary reservation). Verified via the embedder: reserving 512 GiB
  (impossible as an eager allocation on the dev box) and touching a few pages across
  it — including a region at a 400 GiB hint — grows RSS < 20 MiB
  (`x86jit-linux::hostmem::tests::reserved_span_is_sparse_and_reaches_high_addresses`).
- The existing corpus is untouched — `Flat` and its codegen are byte-for-byte as
  before; `Reserved` is opt-in per `Vm`. The boundary tripwire stays green: core's
  dependency set is still exactly `{iced-x86}`.

## Negative consequences

- **No per-page guest protection for `Reserved`** — a `PROT_NONE` reservation is
  indistinguishable from RW backing (both are just addresses in the mapping).
  Acceptable: the engine never enforced `Prot`, and guest `mprotect` is already a
  no-op. A guest relying on a fault from a `PROT_NONE` access would not get one.
  Revisit only if a guest needs real per-page faults (would supersede this ADR with a
  true SoftMmu).
- **Fork is unsupported for a host-backed `Reserved` memory** — `deep_copy` panics,
  because the core can't re-allocate the embedder's mapping and cloning the span
  would commit it. Go never forks; forking guests (busybox) use `Flat`. A `Vec`-backed
  `Reserved` (tests) still forks via a region copy.
- On a host with a small VA (39-bit aarch64) or strict overcommit, a 1 TiB `Reserved`
  span may fail to map; the embedder chooses `span`, so this is a configuration error
  surfaced as a panic, not silent corruption.

## Links

- `go-caddy-plan.md` Phase 1 (BigFlat) — the motivating requirement + DoD.
- `x86jit-core/src/memory.rs` (`Reserved`, `HostRam`, `Backing`, `CODE_WINDOW`),
  `x86jit-core/src/vm.rs` (`with_backend_host_ram`),
  `x86jit-linux/src/hostmem.rs` (the `MAP_NORESERVE` provider).
- `x86jit-tests/tests/boundary.rs` — the tripwire that forced the embedder split.
- spec §4.1 (the `SoftMmu` slot this fills with the cheapest sufficient implementation).
