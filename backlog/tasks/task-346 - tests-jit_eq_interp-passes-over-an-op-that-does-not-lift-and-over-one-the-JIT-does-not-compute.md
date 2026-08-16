---
id: TASK-346
title: >-
  tests: jit_eq_interp passes over an op that does not lift, and over one the
  JIT does not compute
status: To Do
assignee: []
created_date: '2026-08-15 13:26'
labels:
  - code-review
dependencies: []
priority: high
ordinal: 382000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Two ways the workhorse helper reports success without testing anything. 180 of the 214 #[test] fns in x86jit-tests/tests/jit.rs call jit_eq_interp* and contain no assert, no Exit check, no native run and no unicorn leg (counted by parsing the file and classifying each body).

A. It never asserts the snippet reached its `hlt`. When the op under test does not lift, BOTH tiers trap at the snippet's first instruction and the comparator reports identical state:

    vpmovm2d   exits: UnknownInstruction{addr:4096} / UnknownInstruction{addr:4096}   verdict PASS
    ud2        exits: Exception{addr:4096,vector:6} / Exception{...}                  verdict PASS
    mov eax,5  exits: Hlt / Hlt                                                       verdict PASS

Named-test negative control: making the Pblendw arm in lift/mod.rs:1635 return Err(unsupported_insn(insn)) -- pblendw no longer lifts at all -- leaves `pblendw_match_interp` GREEN.

The counterweight, and it is real: `coverage_lists_have_no_stale_entries` DID fire on that break ("stale coverage-list entries: Pblendw"), so a TOTAL loss of a mnemonic is caught elsewhere. A PARTIAL loss -- one encoding, one operand form, a masked EVEX variant while the VEX form still lifts -- is caught by neither. For the 71 mnemonics the ratchet cannot see, the stale check does not apply either.

B. ~70 helper families in codegen/mod.rs:50-114 are C-ABI calls straight into interpreter code, so for those ops jit == interp tests marshalling and never semantics. Negative control: corrupting the shared core `interp::pcmpstrm_run_bv` to return `mask ^ 1` leaves `sse42_pcmpstrm_match_interp` green while `native_pcmpistrm_matches_interp` FAILS.

For pcmpstrm the ALLOWLIST comment correctly pairs the jit test with the native one, so coverage holds. The defect is the ALLOWLIST entries that cite ONLY a jit==interp test: "Pblendw", "Pmuldq"/"Pmulhuw"/"Pmulhw"/"Pmullw", "Vpcmpeqq"/"Vpcmpgtq", "Vpsllv*"/"Vpsrav*"/"Vpsrlv*", plus the uncommented "Vpermt2d/q/w", "Valignd/q", "Vpternlogd/q", "Vpconflictd/q", "Vplzcntd/q", "Vprold/q", "Vshuff32x4/64x2". Several DO have native tests, so the code is better covered than the comments say -- but the comment is what a reviewer reads, and it names a check that cannot fail.

Aggravating context: native.rs is cfg'd to x86_64+linux, and this Unicorn 2.1.4 build cannot run AVX at all (vpcmpeqd ymm0,ymm0,ymm0 -> UnmappedMemory{addr:4096}, confirmed by execution). On the AArch64 runner -- the stated primary target -- neither oracle exists, so the whole AVX/AVX-512 correctness story there rests on exactly these two mechanisms.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 jit_eq_interp asserts the run reached its terminator, so an unlifted op fails instead of passing
- [ ] #2 A test that pins an op whose JIT lowering is a call into interpreter code says so, and names the oracle that actually checks its semantics
- [ ] #3 ALLOWLIST entries citing only a jit==interp test are corrected to name a real oracle or are re-justified
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
