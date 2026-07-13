---
id: TASK-240
title: >-
  lift: MOVMSKPD (66 0F 50) — extract packed-double sign mask to GPR (traps
  today)
status: Done
assignee: []
created_date: '2026-07-13 11:28'
updated_date: '2026-07-13 11:38'
labels:
  - lift
  - simd
  - sse2
  - real-software
dependencies: []
ordinal: 269000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
UnknownInstruction trap hit while running real software (Doom/doomgeneric on the unemups4 PS4 emulator, in-gameplay). x86jit has no lift for MOVMSKPD. Concrete fault: 'movmskpd %xmm0,%esi', bytes 66 0F 50 F0, at guest rip 0x41fdf2. MOVMSKPD (SSE2) extracts the sign bit of each of the 2 packed doubles in the xmm source into the low 2 bits of a GPR (zero-extended). Semantics: dst[0]=src[63], dst[1]=src[127], dst[2+]=0. The sibling MOVMSKPS (0F 50, no 66 prefix) does the same for 4 packed singles (low 4 bits) and is very likely needed too — lift both while here. Encoding: reg field = GPR dest, r/m = xmm src (reg-reg form here). Relates to the SIMD-lowering perf work (task-237/239).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 MOVMSKPD (66 0F 50 /r) lifts: GPR dest = 2-bit sign mask of the packed doubles in the xmm src, upper bits zeroed; reg-reg and any addressing forms
- [x] #2 MOVMSKPS (0F 50 /r) lifted too (4-bit single mask) — the common sibling
- [x] #3 Differential test vs a hardware/Unicorn oracle for representative sign patterns (all-neg, all-pos, mixed); interp + cranelift paths agree
- [x] #4 The exact faulting case 'movmskpd %xmm0,%esi' (66 0F 50 F0) no longer traps
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done. MOVMSKPS/MOVMSKPD lifted (SSE2 + VEX.128). New VMoveMaskFp{dst,src,elem} IR op mirroring VMoveMaskB (pmovmskb): vhigh_bits on I32X4(ps,elem=4)/I64X2(pd,elem=8) -> sign-mask into GPR, upper zeroed. lift movmskps/pd + VEX; native codegen + interp. YMM(256) source deferred (reg_xmm None). Exact Doom fault 'movmskpd %xmm0,%esi' (66 0F 50 F0) no longer traps. Tests: movmsk_ps_pd_match_unicorn (interp vs CPU: all-neg/all-pos/mixed, incl faulting encoding), movmsk_ps_pd_match_interp (jit==interp). Adversarial review: no bugs (incl -0.0/signed-NaN edges safe — raw sign bit). coverage.json regen; ratchet allowlist updated. DoD: nextest --features unicorn 671 passed; clippy+fmt clean.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
