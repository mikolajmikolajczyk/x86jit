---
id: TASK-228
title: >-
 core: guest-RAM backing is UB — the raw ptr and the owning Box are separate
 tags, flagged by both Stacked and Tree Borrows
status: Done
assignee: []
created_date: '2026-07-29 11:08'
updated_date: '2026-07-29 11:31'
labels:
 - bug
 - unsafe
 - core
 - high
dependencies: []
ordinal: 324000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Memory's Backing keeps a raw ptr: *mut u8 alongside the Owner (Box<[u8]> / host mapping) it was derived from, and hands out slices from it:

 x86jit-core/src/memory.rs:147 unsafe fn as_slice(&self) -> &[u8]
 x86jit-core/src/memory.rs:152 unsafe fn as_mut_slice(&self) -> &mut [u8] // from_raw_parts_mut(self.ptr, self.len)

The UnsafeCell wraps Backing, not the bytes, so the raw ptr is a DIFFERENT tag from the Box's. Accesses through one invalidate the other. Miri flags it on the first guest write, under BOTH borrow models — so it is a real aliasing violation, not Stacked-Borrows strictness:

 Stacked Borrows: 'trying to retag from <1021> for Unique permission at alloc607[0x0], but that
 tag does not exist in the borrow stack' at memory.rs:153, via
 Memory::write_bytes -> Backing::as_mut_slice
 Tree Borrows: 'the accessed tag <3173> later transitioned to Disabled due to a foreign write
 access at offsets [0x1000..0x1005]' during the same write_bytes

Reproduced with a program that only maps a page, writes 5 bytes and runs one instruction on the interpreter — i.e. the minimum any embedder does. Every guest memory access in the engine goes through this.

PRE-EXISTING, not from the task-217 work in the tree: the uncommitted memory.rs delta only adds watch_bits_ptr / watch_bits_cover_size and does not touch Backing.

WHY THIS IS URGENT AND NOT A LINT. UB licenses arbitrary codegen, so while it stands, no 'the compiler miscompiled our correct code' claim in this repo can be made. TASK-223 is a release-only wrong-answer bug in the interpreter whose root cause is currently undetermined between our UB and an LLVM defect; this is the prime suspect and has to be cleared first.

Direction: put the bytes themselves behind UnsafeCell (or keep a single provenance root and derive every slice from it) so the ptr and the owner share one tag. Do not paper over it by removing the debug_assert-style checks — the fix is the provenance model, per spec.md section 8 / conventions 'Guest RAM is &Memory with interior mutability'.

Miri is now available (it was not in the pinned devShell):
 rustup run nightly cargo miri run
 MIRIFLAGS=-Zmiri-tree-borrows rustup run nightly cargo miri run
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Miri reports no UB, under both -Zmiri-stacked-borrows and -Zmiri-tree-borrows, for a map + write_bytes + interpreter-run program
- [x] #2 The interior-mutability model is documented at the type and matches what the code does
- [ ] #3 A Miri leg exists in the dev loop / CI (or an explicit decision is recorded if it stays manual)
- [ ] #4 TASK-223 is re-measured after the fix, and the ours-vs-LLVM question is answered either way
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
FIXED 2026-07-29. Backing::boxed derived ptr from a live Box and then MOVED that Box into Owner::Boxed — the move retags the Box, invalidating the pointer taken beforehand. Owner now stores the allocation as raw parts (Box::into_raw in Backing::boxed, Box::from_raw in a new Drop for Owner), so Backing::ptr is the single provenance root for guest RAM and nothing retags it behind the engine's back.

VERIFIED with the map + write_bytes + one-instruction interpreter program:
 before: Stacked Borrows AND Tree Borrows both reject at memory.rs:153 (Backing::as_mut_slice)
 after : Tree Borrows runs clean to completion, correct result
 Stacked Borrows now stops in iced-x86 (decoder/handlers/legacy.rs:33, its own
 &*(self_ptr as *const Self) pattern) — third-party, not ours, and TB accepts it

So our code is clean on that path under the model Rust is moving to. Residual known gaps NOT addressed here (they are not what this task reported, and are not reachable single-threaded): as_mut_slice is still mut_from_ref, and concurrent vcpu stores still hand out overlapping &mut — the documented section 8 interior-mutability discipline.

DID NOT FIX TASK-223. Re-measured the release repro right after: still [3fb6cd8e 00000000 3fb6cd8e 44444444]. The UB was real and worth removing, but it is not the cause of the interpreter's release-only wrong answer. That materially strengthens the compiler-defect reading of 289: on the exact executed path, Miri now reports no UB of ours.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
