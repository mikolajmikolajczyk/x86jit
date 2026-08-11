---
id: TASK-325
title: 'Oracle blind spots: memory operand forms and uncaptured architectural state'
status: Done
assignee: []
created_date: '2026-08-11 11:03'
updated_date: '2026-08-11 21:35'
labels: []
dependencies: []
priority: medium
ordinal: 361000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Three findings that share one consequence: a class of defect this project cannot currently detect, so the coverage it reports is an upper bound rather than a measurement.

**The compat probe instantiates register forms only** (compat.rs ~193). Every *_or_mem / *_rm operand kind is built as a register, and iced puts both alternatives under one Code — so lifting the register encoding marks the whole Code lifted. Memory-only shapes fall into the unencodable bucket and vanish. A generator-emitted caveat now says so in the published map (task-312); the probe is still wrong.

**The AVX fuzz campaign generates no memory operands** (fuzz.rs ~1358). The VEX pool's common shape emits YMM registers and the explicit entries follow it, so the campaign cannot falsify memory-source decoding, effective-address computation, load width, alignment or fault behaviour for anything it counts as covered.

Those two compound: the map says a Code is covered on the strength of its register form, and the fuzzer only exercises register forms. Memory-form gaps are invisible to both, which is how vextract*'s memory destination survived until a real PS4 binary trapped on it.

**The native oracle captures a subset of architectural state** (native.rs ~541). x87 state is defaulted, the snapshot model has no MXCSR, and vector capture stops at register 15. A snippet can set the wrong rounding mode, raise the wrong FP exception flags, corrupt the x87 status or tag word, or write ZMM16-31 wrongly and still match every compared field. The README now states this narrowing (task-313).

Merged from task-320, 311, 321.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Register and memory forms are probed independently; a Code is fully lifted only when every form lifts, otherwise partial naming the failing form
- [x] #2 The fuzzer selects register and memory forms independently, varying alignment and page boundaries, and reports coverage per operand form
- [x] #3 The regenerated coverage map's newly-revealed gaps are triaged
- [x] #4 MXCSR, full x87 state and ZMM16-31 are captured and compared
- [x] #5 The generator caveat and the README narrowing added by task-312/313 are removed once each stops being true
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
PART 1 OF 3 DONE (b9a9543): the compat probe builds memory forms; 194 long-mode codes were over-reported as covered and are now the reg_only column. Two side-effects of the same fix - lddqu was not lifted at all while CPUID advertises SSE3, and 55 memory-operand mnemonics had never been visible to the coverage ratchet.

PARTS 2 AND 3 DONE. Suite 767 -> 779 tests, all green; clippy, fmt, aarch64 cross-check, cargo deny, guest-agnostic guard and the FULL 169-rung ladder clean.

(2) THE FUZZER NOW GENERATES MEMORY OPERANDS. VexOp gained emit_mem; the r3! macro produces it for free, so 43 of the 63 pool entries got a memory form with no extra code, and 15 of the 20 hand-written entries got one by hand (vpslldq/vpsrldq have no memory form at all). New campaign leg: `cargo xfuzz --mem`, and CampaignCfg::mem_forms. Built as a REWRITE of gen_avx's output, not a new draw: adding a reg-or-mem draw inside the generator would have shifted the RNG stream and silently changed what every recorded seed means (TASK-326's seed 26816 among them). So the memory campaign is the same programs, and a divergence that appears there but not in gen_avx at the same seed is in the memory path. New two-page VSCRATCH region at 0x240000 filled with a varied pattern, so operands can be 32-byte aligned, arbitrarily unaligned, or straddling the page boundary - and so a wrong load width is visible at all (a region of zeros cannot show one). SCRATCH_LEN is untouched because it feeds rng.below() draws.

WHAT IT FOUND IN ITS FIRST 90 SECONDS - one real wrong-result defect, FIXED: lift_vpblendw issued a fixed 16-byte VLoad for its memory form, so `vpblendw ymm, ymm, m256` blended its high lane against whatever the destination already held. Wrong silently, no trap. Both tiers share the lift, so jit_eq_interp agreed with itself; only the real CPU disagreed. Found at seed 206, fixed with the VLoadWide idiom already used by lift_vpermil, pinned by native_vpblendw_ymm_reads_its_whole_memory_source and verified by reverting the fix.

Plus 8 unlifted memory forms (vpermilps, vpermilpd, vpermps, vpshufhw, vpshuflw, vmpsadbw, vcvtps2ph memory-destination) which trap rather than mislift. TRIAGE (AC#3): every one of them is in the coverage map's reg_only list, so the two tools independently agree on the same gap set. They are not new defects, they are the 194 reg_only codes seen from the other side.

NOT OURS, filed on TASK-326: seed 5915 is a jit != interp hard-invariant violation on roundpd - the interpreter does not quiet a signalling NaN (7ff0000000000001 stays, hardware and the JIT both give 7ff8000000000001). Reproduces identically WITHOUT --mem, so it is pre-existing and unrelated to this task.

(3) THE NATIVE ORACLE CAPTURES THE STATE IT WAS DEFAULTING. x87 register stack, control word, status word and tag word now come out of the signal frame's legacy FXSAVE area (SDM Vol 1 sec 10.5.1, Table 10-2); MXCSR with them. The child's x87 is established by fxrstor from an image the parent builds, because fninit alone leaves the data registers unchanged (SDM Vol 2A FINIT/FNINIT) - the child was inheriting the parent's dirty x87 stack. Tag word is written all-empty, the state a process starts with, which makes a guest fld push the way the interpreter models it. ZMM16-31 come from the XSAVE Hi16_ZMM component (SDM Vol 1 sec 13.5.5) and the stub now zeroes zmm16-31 as well.

CpuSnapshot widened from 16 vector registers to 32 (VREGS) - CpuState has had 32 since M8, so everything an EVEX instruction wrote above register 15 was invisible to every oracle AND to the comparator, which looped to 16. MXCSR added to the snapshot, compared on its CONTROL half only (MXCSR_CONTROL_MASK): the six sticky exception flags are captured and printed in a divergence report but not compared, because hardware sets PE on any inexact result and the engine raises nothing - comparing them would report the deferred gap (deferred.md) on nearly every FP snippet. deferred.md and PROVENANCE.md now say exactly that.

METHOD NOTE WORTH KEEPING. The first zmm16-31 test passed with the comparator still narrowed to 16 AND the interpreter dropping zmm20 - because it asserted on the oracle's captured value itself, and the compare() assertion silently ignored registers >= 16. Asserting that an oracle CAPTURED something does not test that the comparator LOOKS at it. Fixed by driving compare() directly, one register and one field at a time (compare::state_width_tests). Every fix in this task was negative-controlled by breaking it and watching the test fail.

WHILE HERE, two published claims were false and are corrected: the coverage map's own header note promised a reg_only/missing_mem_form column that the markdown writer never emitted (the fields were computed and thrown away) - now rendered, with the 194 codes listed by name; and PROVENANCE.md sec 3 plus status.md still carried "known defect: F80::div is off by 1 ULP", fixed in 4acac26.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
