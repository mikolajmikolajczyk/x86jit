---
id: TASK-165
title: >-
  interp: plain guest store racing an atomic RMW/CAS breaks mutual exclusion
  (as_mut_slice UB)
status: Done
assignee: []
created_date: '2026-07-08 04:48'
updated_date: '2026-07-08 04:58'
labels:
  - 'crate:core'
  - 'goal:fix'
  - mt
dependencies: []
ordinal: 174000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Under InterpreterBackend, a plain guest store (Memory::write) racing a concurrent atomic RMW/CAS (atomic_rmw/atomic_cas) on the SAME location loses coherence: the plain store is reordered/elided relative to the atomic op, breaking mutual exclusion. A textbook-correct x86 spinlock — atomic `lock cmpxchg` acquire + PLAIN-STORE release — fails to exclude at 8 vcpus (two threads enter the critical section; a plain `inc` in the CS loses updates). Swapping the release to an atomic `xchg` fixes it, and swapping the CS `inc` to `lock inc` masks it — both prove concurrent CS entry.

ROOT CAUSE: Memory::write/read went through `as_mut_slice()`/`as_slice()`, creating a `&mut [u8]`/`&[u8]` over the WHOLE backing while other vcpus hold atomic references into it — mutable-aliasing UB. The optimizer, assuming no aliasing, reorders/elides the plain store vs the atomic CAS. (Also: runtime-length copy_from_slice isn't single-copy atomic.) The JIT is immune — it emits real host movs, no Rust slice aliasing.

FIX (validated): route scalar Memory::read/write through a raw backing pointer + AtomicU{8,16,32,64} Relaxed load/store for naturally-aligned 1/2/4/8-byte access (byte-copy fallback for misaligned) — same raw-pointer discipline atomic_rmw/atomic_cas already use. On x86 a Relaxed atomic load/store lowers to the SAME plain mov, so ZERO runtime cost; it only removes the UB so the compiler stops reordering. Patch: scratchpad/scalar-atomic-fix.patch (this session).

REPRO (deterministic): x86jit-tests atomics probe cmpxchg_spinlock_counter — 8 vcpus, cmpxchg-acquire + plain-store release, plain inc in CS, assert count==THREADS*ITERS. FAILS 3/3 at 8 threads on HEAD, PASSES with the fix. cas_increment_counter + lock_xor_binary_path (also in the probe) pass on both (direct atomic RMW/CAS is fine).

NOT task-161: this does NOT fix the caddy corruption (Go's lock release uses atomic xchg, dodging this exact manifestation; caddy fixed-arm 9/25 vs baseline 18/40, p~0.46 — no effect). Separate, independent MT-correctness bug. Full suite 255/255 green with the fix.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
