---
id: TASK-329
title: JIT-compiled guest stores must invalidate translated pages
status: Done
assignee: []
created_date: '2026-08-13 12:01'
updated_date: '2026-08-13 16:38'
labels:
  - m6-smc
dependencies: []
priority: high
ordinal: 365000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
MEASURED, not inferred: with `JitBackend`, a guest that patches another block's bytes and then calls it runs the STALE translation. The same program on the interpreter observes the patch.

    interp: exit=Hlt eax=2 misses=5   (coherent)
    jit:    exit=Hlt eax=1 misses=4   (stale block ran)

Cause: the JIT inlines a store straight into host RAM and calls at most `Memory::note_watched_write` (memory.rs:676), whose own doc comment says 'WITHOUT the SMC code-page check'. `note_write` (memory.rs:650) is the only thing that sets a page dirty, and it is reachable only from `Memory::write*`/`atomic_*` — the interpreter and the embedder. So no compiled store has ever invalidated anything.

This is NOT a multi-vcpu defect; one vcpu reproduces it. Split out of task-323 AC#5, whose framing ('single-vcpu execution is unaffected') does not hold for it.

Why the suite is green over it: the only guest-self-patch tests are interpreter-only (smc.rs:46). Every JIT-backed SMC test writes from the EMBEDDER side (`embedder_rewrite_reexecutes_jit`, `stale_link_slot_cleared_on_invalidation`), which routes through `Memory::write` and therefore never exercises the compiled store path. The gap is structurally invisible to the tests that look like they cover it.

Two published claims are false because of it, and must be corrected either way:
- README.md:90 'Self-modifying code stays coherent' — untrue on the default backend.
- deferred.md:49 predates M6 ('Nothing needs it until a guest modifies its own code').

spec.md §10 defers only SAME-BLOCK SMC ('the running block keeps executing the old code to its end'). It does not license a backend that never observes a guest store to a code page at all — its main bullet is 'On a write to such a page -> remove the affected entries from the cache'. The implementation is weaker than its own spec.

DESIGN NOTE for whoever picks this up. `note_watched_store` (codegen/mod.rs:2568) is the shape to mirror: an inline per-page test with the helper call laid out in a cold block. But do NOT copy its table load. The watch path can gate on `watch_count != 0`, which is usually zero; code pages always exist, so that gate has no SMC equivalent. Use a WATERMARK instead — the min/max code page ever marked, carried in `MemCtx` — so the hot path is one subtract and one unsigned compare (`(page - code_lo) < code_len`) that stack and heap stores fall straight through. task-217's first cut was reverted for putting a table probe in the hot stream and doubling a store's emitted code; a watermark is what avoids repeating that.

Related, found by the same probe and NOT part of this task: `Prot` is not enforced on stores by EITHER backend — a write to a `Prot::RX` region succeeds and changes the bytes. The engine models no permission faults at all. Undocumented; file separately.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A guest that patches another block's bytes under JitBackend observes the patch, matching the interpreter
- [x] #2 The hot store path cost is MEASURED against bench/baseline.json before and after, and the number is recorded on this task
- [x] #3 The self-patch test runs on BOTH backends from one body, so neither can regress silently
- [x] #4 A page-straddling store, and a store to the first and last page of the code range, are covered
- [x] #5 README.md:90 and deferred.md:49 state what is actually true
- [x] #6 spec.md §10 distinguishes the deferred same-block case from the cross-block case this closes
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed on the working tree 2026-08-13; NOT yet committed.

WHAT IT WAS. Compiled stores reached no SMC hook at all. Measured, both backends, same program:
  interp: eax=2 misses=5   (coherent)
  jit:    eax=1 misses=4   (stale block ran)

TWO defects, not one. The write barrier was only half. `Vm::handle_smc` runs in the OUTER dispatcher loop, and the compiled inner loop follows chain/link/IBTC edges without returning to it — so even with the page marked dirty the guest transferred into the stale translation. Proof it was a second, separate defect: with the barrier in and budget=None the probe still read eax=1, with budget=Some(1) (dispatcher every block) it read eax=2. The inner loop now leaves the chain on `Memory::has_dirty_code`.

THREE write paths needed it, not one. The inline store gate, and then — found by the new both-backends tests, not by reading — the string helper (`rep stos`, whose reporting was gated on `watch_count != 0`, i.e. off for every guest that watches nothing) and the x87 helper (`fistp`, which glibc's number formatting really uses). Both write through raw bounds-checked-only views by design. `fxsave` too. New `x87::mem_write_bytes` gives the helper the width to report and sits beside `exec_x87` so the two lists get read together.

DESIGN. `Memory::code_range`: one `AtomicU64` packed `(lo << 32) | len` in BYTE addresses, read live through `MemCtx.code_range_ptr` (offset 120, append-only). One word so a torn read cannot produce a NARROWED range, which would silently skip a page. `lo` is kept one page low, which is what lets the test look at the store's first byte only and still catch one that spills into the range's first page — a store is at most 64 bytes against a 4096-byte page. Byte addresses fit 32 bits because `code_page` is capped at CODE_WINDOW.

AC#2 — THE MEASUREMENT.
  density (marginal hot instructions per store): 10.3 -> 19.5. First cut was 22.9; splitting the two gates into separate branches (the `bor` made the backend materialize both predicates with setnz/setb/orl/testb) and replacing a constant-pool `and` with a 32-bit move took off 3.4.
  wall clock, perf-gate vs e11a888, two runs: memcpy +9.4% then +12.0% — the stable signal, and the only store-heavy workload. hotloop read +30.9% then +4.8%: NOISE, its band is 13-16%. simd/indirect/fib32 inside their bands. New baseline recorded (memcpy jit run 5.69 -> 6.34ms).
  Ratchet raised 14 -> 21 with the rationale REWRITTEN — it described only the watch test and would have read as a rubber stamp. It now carries the four-row history and says what it still guards.
  Maintainer accepted the cost deliberately; TASK-331 carries the zero-cost redesign (host page protection).

NEGATIVE CONTROLS, all five fail as they must: inline gate removed, chain-break removed, one-page skew removed, x87 reporting removed, string helper reverted to the watch gate.

VERIFIED: 804/804 debug AND release, clippy --all-features, fmt, aarch64 cross-check, guest-agnostic guard, cargo deny, and the FULL 169-rung ladder (busybox, sqlite, lua, CPython, Go, caddy).

ALSO CORRECTED while here — README 'Known gaps' cited THREE task ids that no longer exist (TASK-234, TASK-314, TASK-306, all consumed by the renumbering) and claimed the x87 control word does not reach arithmetic, which TASK-324 fixed. Rewritten against TASK-328/TASK-323.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
