---
id: TASK-305
title: Trapped instructions must be resumable and leave precise state
status: In Progress
assignee: []
created_date: '2026-08-10 15:37'
updated_date: '2026-08-13 17:10'
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
- [ ] #2 Every subtask closed
- [x] #3 A native fault-and-retry witness exists for at least the 256-bit and the cross-page cases
- [x] #4 conventions.md states the invariant and why a differential test cannot check it
- [x] #5 No 256-bit handler writes its destination before both memory halves have loaded
- [x] #6 Every faulting memory-source vector op carries its first source explicitly; VHFloatM is the documented pattern
- [x] #7 The faulting sub-address (and for writes its width and value) reaches Exit::UnmappedMemory
- [x] #8 Native fault-and-retry witnesses exist for the 256-bit dst==src1 case and the cross-page case
- [ ] #9 A vector load/store to a Trap region completes after the embedder answers, or is refused with a defined error rather than looping
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed on the working tree 2026-08-13, NOT yet committed. AC#9 split to TASK-332; AC#2 is the umbrella.

ONE PROBE CAUGHT BOTH BACKENDS FAILING A DIFFERENT HALF — 'vaddps ymm0, ymm0, [mem]' with the high 16 bytes past the guest span:
  interp: UnmappedMemory{addr: 0x10000}  xmm0 1.0 -> 3.0   <- destination COMMITTED before the fault
  jit:    UnmappedMemory{addr: 0xfff0}   xmm0 1.0 -> 1.0   <- operand BASE, not the faulting half
Neither tier could see the other's bug, which is why jit_eq_interp never did.

AC#5 — 11 interp handlers wrote guest state before their last faulting load, found by walking every function with >=2 vloads rather than by reading the four the reviews named: exec_v_p_round_m, exec_v_logic256_m, exec_v_packed_bin256_m, exec_v_pmadd_m, exec_v_pshufb256_m, exec_v_float_bin256_m, exec_v_float_unary256_m, exec_v_packed_cvt256_m, exec_v_shufps256_m, exec_v_unpack256_m, exec_v_float_cmp_mask256_m. All compute into locals and commit together.

AC#6 — VPackWideM/VUnpackLowM/VHIntM now carry 'a' explicitly, matching VHFloatM; the lift's pre-copy is gone from all three. The pre-copy was a WRITE TO THE DESTINATION placed before the faulting load. Note what it did NOT do: the retry result stayed correct, because 'a' survives in its own register. What was wrong is the architectural state AT the fault, which is what a debugger or a guest fault handler reads. Both backends failed the witness before the fix.

AC#7 — two separate defects.
  JIT: one checked_addr covers the whole [addr, addr+size), so it named the base. Now reports the first unbacked byte and the width of the unbacked tail, computed INSIDE the fault block so the hot path is untouched.
  interp: vload/vstore do two 8-byte accesses and returned a bare MemTrap, so callers reported the 16-byte base. Error type is now (u64, MemTrap); 54 match arms plus 5 hand-written sites, all found by the compiler after the type change.

AC#8 — x86jit-tests/tests/fault_atomicity.rs, 8 tests. The resumable one (fault -> embedder maps -> re-run -> a+b exactly once) is interpreter-ONLY and says why on itself: resuming needs the address to become valid, hence in-span, and in-span the JIT does not fault at all — MEASURED (Hlt on the JIT, UnmappedMemory on the interp). That is decision-3, not a gap. The JIT is held to the other two properties.

AC#1/#4 — conventions.md gained a 'Fault atomicity — commit last' section: the invariant, the three shapes that break it, and why a differential test cannot check any of it (both tiers share the IR, and it compares state after a COMPLETED run — a trapped instruction's partial state is exactly what it never looks at).

A PUBLISHED CONTRACT CHANGED, deliberately: jit.rs::extract_lane_mem_dst_straddle_fault_match_interp asserted 'interp must fault at the store base'. Both tiers now name the unbacked half. Old behaviour told the embedder to map a page it already had — map, retry, fault, loop, with no way out. Test and its doc comment updated to the new contract with that reasoning.

NEGATIVE CONTROLS, four, all failing as they must. One initially did not: reverting vload's sub-address left every test green, because the vextract straddle test stores lane-by-lane and the 256-bit tests split at 16 bytes — NOTHING covered the inner 8-byte half. Added that assertion to three_operand_vex_mem_leaves_dst_untouched before re-running. Exactly the 'a check that cannot fail reports clean' shape.

VERIFIED: 812/812 debug and release, clippy --all-features, fmt, aarch64 cross-check, guest-agnostic guard, perf gate OK.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
