---
id: TASK-305
title: Trapped instructions must be resumable and leave precise state
status: Done
assignee: []
created_date: '2026-08-10 15:37'
updated_date: '2026-08-13 17:58'
labels: []
dependencies: []
priority: high
ordinal: 337000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A precise fault leaves the destination unchanged. Three places in this engine break that rule, and an adversarial review found each independently without noticing they are one invariant — which is the argument for one contract rather than three patches.

The rule to establish, write into conventions.md, and then enforce: an instruction may not commit any guest-visible state until every read that can fault has succeeded. Compute into temporaries; commit last.

**interp, 256-bit memory ops** (interp/vector.rs ~3317). The low 128-bit result is stored, then the high memory half is loaded. If that faults, rip is rewound but the destination keeps the partial result — and with the legal dst==src1 aliasing the retry reads its own output as the source, so 'vaddps ymm0, ymm0, [mem]' double-adds the low lanes once the page is mapped. The shape repeats across the 256-bit logic, integer, conversion, shuffle and compare handlers.

**IR, faulting memory-source ops** (ir.rs ~681). VPackWideM omits its first source and documents that the lifter must copy it into dst *before* the faulting operation. VUnpackLowM and VHIntM repeat it. VHFloatM already carries the source explicitly and shows the shape that works. Fixing the interpreter without fixing the op leaves the next backend free to reintroduce it.

**vload/vstore fault addresses** (interp/mod.rs ~4888). A 16-byte access is two 8-byte operations but only MemTrap comes back, so callers report the 16-byte base. For an unaligned access whose first half is mapped and second is not, the embedder is told to map a page it already mapped, retries, faults identically, and loops. It cannot work around this: the information was discarded before the Exit was built.

**Vector MMIO exits can never be completed** (interp/vector.rs ~17). Vector loads and stores re-call vload/vstore unconditionally after an MMIO exit and never consume the value or acknowledgement complete_mmio_read/complete_mmio_write installed, so the retry produces the same exit forever. A 16-byte store additionally exposes only `v as u64` while declaring size == 16, handing the embedder half a transaction and calling it whole. Same contract as the rest of this task from the embedder's side: an instruction that trapped must be able to finish. Merged from task-317.

Note what cannot validate any of it — jit-vs-interp comparison, because both tiers share the shape. It needs native fault/retry witnesses. Merged from task-305.1/.2/.3.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The invariant is stated in conventions.md with the reason a differential test cannot check it
- [x] #2 Every subtask closed
- [x] #3 A native fault-and-retry witness exists for at least the 256-bit and the cross-page cases
- [x] #4 conventions.md states the invariant and why a differential test cannot check it
- [x] #5 No 256-bit handler writes its destination before both memory halves have loaded
- [x] #6 Every faulting memory-source vector op carries its first source explicitly; VHFloatM is the documented pattern
- [x] #7 The faulting sub-address (and for writes its width and value) reaches Exit::UnmappedMemory
- [x] #8 Native fault-and-retry witnesses exist for the 256-bit dst==src1 case and the cross-page case
- [x] #9 A vector load/store to a Trap region completes after the embedder answers, or is refused with a defined error rather than looping
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#9 CLOSED after all, in the same session, by TASK-332 — see that task. The reason it was split was that changing the exit shape looked like an embedder-contract decision; it is not, because the previous shape was an infinite loop and nothing can depend on behaviour that hangs. A vector access to a Trap region now completes transfer by transfer on both backends.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
