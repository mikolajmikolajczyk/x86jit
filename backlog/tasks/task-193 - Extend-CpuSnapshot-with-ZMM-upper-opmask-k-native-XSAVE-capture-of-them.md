---
id: TASK-193
title: Extend CpuSnapshot with ZMM upper + opmask (k) + native XSAVE capture of them
status: Done
assignee: []
created_date: '2026-07-09 17:15'
updated_date: '2026-07-09 19:26'
labels:
  - code-review
  - 'crate:tests'
  - 'goal:test'
dependencies: []
ordinal: 217000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-191 captured YMM upper halves into RunOutcome.ymm_hi, but the test harness CpuSnapshot (x86jit-tests/src/vector.rs) tops out at YMM — no zmm_hi/kmask fields — so ZMM (bits 511:256) and the opmask k0-k7 have nowhere to be compared. Grow CpuSnapshot with zmm_hi:[[u128;2];16] (or 32) + kmask:[u64;8] (serde like ymm_hi), thread through oracle.rs store_snapshot/load_snapshot (needs Vcpu zmm/kmask getters+setters — state.rs already holds the state), compare.rs (zmm/kmask diffs), unicorn.rs (leave zero — QEMU build can't AVX-512 anyway), and native.rs run_native: capture ZMM_Hi256 (xstate comp 6, offset from cpuid 0xD.6) + Hi16_ZMM (comp 7) + opmask (comp 5) from the signal XSAVE area — the handler infra + host_ymm_offset pattern is already there, add sibling offsets. This is what makes the NativeOracle a true AVX-512 net for 168.5.x. Depends on 191 (done).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 CpuSnapshot carries zmm_hi + kmask (serde round-trips; RON corpus stays compatible via serde default)
- [x] #2 compare.rs diffs zmm_hi + kmask; oracle.rs store/load thread them via Vcpu getters/setters
- [x] #3 run_native captures ZMM_Hi256/Hi16_ZMM + opmask from the signal XSAVE area; an AVX-512 snippet writing zmm upper + a k register is oracled native-vs-interp
- [x] #4 cargo nextest (--features unicorn) green minus fuzz_robustness; clippy -D warnings; fmt --check clean
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Landed. CpuSnapshot grew zmm_hi:[[u128;2];16] (serde zmm_hex, 32 flat hex strings) + kmask:[u64;8] (serde default). oracle.rs load/store_snapshot thread them via Vcpu set_zmm_hi/zmm_hi + set_kmask/kmask. compare.rs diffs zmm_hi (per half) + kmask with display. native.rs captures opmask (XSAVE comp 5, CPUID 0xD.5) + ZMM_Hi256 (comp 6, 0xD.6) from the signal frame; the stub on an AVX-512 host zeroes zmm0-15 (vpxorq) + k0-7 (kxorq) so untouched regs read back 0 = interp zero-init; nonzero zmm_hi/kmask init rejected (stub loads only xmm). unicorn.rs sets them 0 (no AVX-512). Tests: jit avx512_zmm_logic_observable_match_interp (512-bit vpxorq/vpternlogq now compared across all 512 bits — retroactively validates task-168.5.2's 512 path) + native_captures_zmm_and_opmask (vmovdqu64 zmm + kmovw k, captured from real CPU). Host has AVX-512; existing native EVEX-k test still green (kmask now captured). Suite 381/381, clippy+fmt clean. DEFERRED: zmm16-31 (Hi16_ZMM comp 7) not captured — snapshot is 16 regs, matching xmm/ymm width.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
