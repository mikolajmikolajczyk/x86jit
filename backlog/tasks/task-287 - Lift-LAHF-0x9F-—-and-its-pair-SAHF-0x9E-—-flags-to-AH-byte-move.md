---
id: TASK-287
title: Lift LAHF (0x9F) — and its pair SAHF (0x9E) — flags-to-AH byte move
status: In Progress
assignee: []
created_date: '2026-07-22 19:57'
updated_date: '2026-07-22 20:15'
labels: []
dependencies: []
ordinal: 317000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A retail UE4 title (unemups4 CUSA05952) faults with `UnknownInstruction` on `lahf` (single byte 0x9F) at guest rip 0x1c5c88c, inside what looks like a setjmp/unwind sequence: bytes are `9f 48 89 c1 58 48 83 c0 08` = lahf; mov rcx,rax; pop rax; add rax,8.

LAHF loads AH with the low byte of RFLAGS: bit0=CF, bit1=1 (reserved, always set), bit2=PF, bit3=0, bit4=AF, bit5=0, bit6=ZF, bit7=SF. OF is NOT included. SAHF (0x9E) is the inverse — it writes CF/PF/AF/ZF/SF from AH and leaves OF untouched — and the two almost always appear together in save/restore pairs, so lifting only one leaves the other as the next wall.

Both are valid in 64-bit mode on every AMD CPU (and on Intel since Nehalem); the Jaguar cores the PS4 uses set CPUID.80000001H:ECX.LAHF-SAHF[bit 0], so guest code compiled for this target may use them freely.

Note for the interpreter/JIT split: the reserved bit1 must read back as 1, and bits 3/5 as 0, otherwise a subsequent SAHF round-trip through AH will not reproduce the original flags.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 `lahf` lifts in both interpreter and Cranelift backends and sets AH to CF | 0x02 | PF<<2 | AF<<4 | ZF<<6 | SF<<7, leaving OF and all other flags unchanged
- [ ] #2 `sahf` lifts in both backends and restores CF/PF/AF/ZF/SF from AH while leaving OF unchanged
- [ ] #3 A differential test (jit vs interp vs native) round-trips lahf/sahf over a set of flag states covering each of CF/PF/AF/ZF/SF set and clear
- [ ] #4 unemups4 CUSA05952 passes guest rip 0x1c5c88c without an UnknownInstruction fault
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
IMPLEMENTED 2026-07-22. AC#1-#3 done here; AC#4 is embedder-side and unverified from this repo.

WHAT WAS ALREADY THERE. `IrOp::Lahf` / `IrOp::Sahf` and the interpreter's `exec_lahf` /
`exec_sahf` existed for real-mode (§17.6) since the Real16 work. Two things blocked long mode:

  - lift/mod.rs gated both arms on `mode.wraps_16()`, so Long64/Compat32 returned
    UnknownInstruction — the fault the embedder hit at guest rip 0x1c5c88c
  - the Cranelift backend listed both in the `unreachable!("real-mode IR ops never reach the
    JIT")` arm

So the work was ungating the lift and writing the two JIT lowerings; the interpreter's logic
needed no change. `exec_lahf` reads `to_flags16() & 0xFF`, which is the same word below bit 8 in
every mode and masks IF away, so it was already correct for Long64.

JIT LOWERING (x86jit-cranelift/src/codegen/control.rs).

  emit_lahf — assembles CF | 0x02 | PF<<2 | AF<<4 | ZF<<6 | SF<<7, shifts it into AH and merges
  with RAX. OF is not in the byte and no flag is written. PF and AF are stored as SOURCES
  (task-285), so this path DERIVES them via load_pf/load_af rather than loading a bit — `lahf` is
  one of the few readers that keeps them worth storing at all.

  emit_sahf — writes CF/PF/AF/ZF/SF from AH bits 0/2/4/6/7 and leaves OF, DF and IF alone. One
  detail worth keeping: AF's source encoding IS bit 4 of a byte, so `ah & 0x10` is already a valid
  `af_src` and needs no shifting. PF goes through `pf_src_from_bool` because a source byte's EVEN
  parity means PF=1.

RESERVED BITS. AH bit 1 reads back set, bits 3 and 5 clear. Both engines get this from the same
place — the interpreter from `to_flags16`'s literal reserved bit, the JIT from the `iconst(1 << 1)`
seed — and `sahf_lahf_round_trip` pins it, because a wrong reserved bit only shows up after a
round trip through AH, which is exactly what the setjmp/unwind sequences in the report do.

AC#3 — THREE-WAY COVERAGE, via two existing harnesses that compose:
  differential.rs (interp == Unicorn == hardware): sahf_lahf_round_trip_matches_unicorn over AH =
    0x00/0x01/0x04/0x10/0x40/0x80/0xD5/0xFF (each of CF/PF/AF/ZF/SF alone, none, all),
    sahf_leaves_overflow_untouched_vs_unicorn, lahf_captures_computed_flags_vs_unicorn
  jit.rs (jit == interp): lahf_sahf_round_trip, sahf_preserves_overflow,
    lahf_captures_computed_flags

VERIFIED THE TESTS REACH THE JIT, rather than assuming it: mutating emit_lahf's reserved-bit seed
from `1 << 1` to `1 << 3` fails both jit-side lahf tests. Reverted.

COVERAGE RATCHET. Both mnemonics went to ALLOWLIST, not to the fuzzer menu, and the reason is
recorded at the entry: `gen(seed, len)` builds MULTI-INSTRUCTION programs, so a fuzzer-emitted
`lahf` following a shift or a multiply would capture our arbitrary choice for an architecturally
UNDEFINED AF/PF into AH — converting a waived flag difference into a register difference that no
`dont_care` mask can cover. `lockstep.rs` already excludes the pair for the same cause. This is
the ratchet's "last resort" path used deliberately, with hand-written coverage in its place.

COMPAT MAP regenerated (`cargo run -p x86jit-tests --bin compat -- --write`): long64 x86-64-v1
83% -> 84% (106 -> 104 missing), compat32 x86-64-v1 82% -> 83%.

Gates: cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)' 906/906 (was 900 —
the 6 new tests); clippy --all-targets --all-features -D warnings clean; fmt --check clean;
cargo check --target aarch64-unknown-linux-gnu --tests clean.

AC#4 OPEN: needs unemups4 CUSA05952 re-run past guest rip 0x1c5c88c. Cannot be checked from here.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
