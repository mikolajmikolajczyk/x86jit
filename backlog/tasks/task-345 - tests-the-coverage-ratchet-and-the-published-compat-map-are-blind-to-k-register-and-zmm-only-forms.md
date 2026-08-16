---
id: TASK-345
title: >-
  tests: the coverage ratchet and the published compat map are blind to
  k-register and zmm-only forms
status: To Do
assignee: []
created_date: '2026-08-15 13:26'
labels:
  - code-review
dependencies: []
priority: high
ordinal: 381000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Both rest on `x86jit-tests/src/compat.rs::template_operand`, whose final arm is

    // Anything else (mask regs, zmm, vsib, seg/cr/dr/tr/bnd, moffs, rel, wider
    // immediates, is4 for legacy) -- exotic or out of our v1..v3 focus.
    _ => return Err(()),

so every form whose operands are k_reg/k_rm/k_vvvv or zmm_reg/zmm_rm/zmm_vvvv probes Unencodable. The scope gate (`code_gen`) admits AVX512F/BW/DQ/VL/CD, so these ARE in scope; only the templater refuses them.

A. coverage_ratchet.rs:1024-1046 asserts "every mnemonic the lifter handles is either exercised by the differential fuzzer menu or explicitly waived". Measured with an extended templater that adds the k_*/zmm_* arms and is otherwise byte-for-byte the shipped probe:

    lifted_mnemonics() reports 822 mnemonics
    mnemonics the lifter HANDLES but the ratchet cannot see: 71  (from 95 iced Codes)
      the whole K* family (Kand*, Kor*, Kxor*, Knot*, Kmov*, Kshift*, Kunpck*, Kortest*),
      Vbroadcast{f,i}{32x8,64x4}, Vextract/Vinsert {f,i}{32x8,64x4},
      Vpcmp{b,d,q,w,ub,ud,uq,uw}, Vptestm*/Vptestnm*

None of the 71 is in FUZZER_COVERED or ALLOWLIST, and the test is green. 37 of them appear NOWHERE in the test suite -- not in a test name, not in an assembled snippet. That includes the whole `kortest*` family, which writes ZF/CF and is THE AVX-512 branch primitive: no correctness test at any width.

Negative control both ways: deleting "Xadd" from ALLOWLIST makes the ratchet fail naming Xadd. The ratchet works for everything it can see; the hole is the probe's.

B. The same arm makes the PUBLISHED map understate the gaps. The shipped probe files 760 in-scope long-mode Codes as `unencodable` (v4 alone: 591 unencodable vs 702 encodable). Re-probing that set with the extended templater: lifted=166, UNSUPPORTED=71, still-unencodable=523. So backlog/docs/compat/isa-coverage.md's "long64 x86-64-v4 -- missing" section is short by at least 71 entries, among them EVEX_Vcmp{pd,ps,sd,ss}_kr_k1_*, EVEX_Vfpclass*, EVEX_Vpmov{b,d,q,w}2m, EVEX_Vpmovm2*, EVEX_Vpbroadcastm*.

README.md:82-87 names only ONE reason to read the map as an upper bound (register form probed, memory form not). The operand-templater gap is larger and is disclosed only as an unlabelled `unencodable` column.

Concrete consequence of the mnemonic keying: Vcmpps IS in lifted_mnemonics() because its VEX form lifts, while `vcmpps k1, zmm1, zmm2, imm8` probes Unsupported -- so the ratchet credits the op as covered.

A guest sees an AVX-512 binary trapping on an op the published map does not list as missing, or getting a wrong result from a mask op nothing ever validated.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 template_operand synthesizes k-register and zmm operands, so the ratchet sees every mnemonic the lifter handles
- [ ] #2 The 71 mnemonics either gain correctness coverage or an ALLOWLIST entry with a reason -- kortest* first, since it is a branch primitive with none at any width
- [ ] #3 The regenerated compat map lists the previously hidden Unsupported forms as missing rather than unencodable
- [ ] #4 README's upper-bound caveat names the operand-templater limit as well as the memory-form one
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
