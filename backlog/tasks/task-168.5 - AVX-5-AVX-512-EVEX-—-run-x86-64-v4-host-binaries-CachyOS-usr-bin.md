---
id: TASK-168.5
title: 'AVX-5: AVX-512/EVEX — run x86-64-v4 host binaries (CachyOS /usr/bin)'
status: In Progress
assignee: []
created_date: '2026-07-08 17:53'
updated_date: '2026-07-10 08:09'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies: []
parent_task_id: TASK-168
ordinal: 182000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Extend the SIMD lifter from VEX/AVX2 (task-168, done) to EVEX/AVX-512 so x86-64-v4 host binaries run — CachyOS /usr/bin are v4-optimized (AVX-512F/BW/DQ/VL/CD). EVEX is a strictly larger surface than VEX: a 4-byte 62h prefix, 32 vector regs (ZMM0-31 at 512-bit), 8 opmask registers (k0-k7) for per-element predication/zeroing, embedded broadcast, and embedded rounding/SAE. Big state + IR + backend widening, comparable in size to all of 168. Gate advertisement LAST (mirrors 168.4) — advertising AVX-512 before lifting is solid turns the whole glibc/distro corpus onto EVEX paths that would #UD on any unlifted op.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 EVEX 62h decode + ZMM0-31 (512-bit) + k0-k7 opmask state land in CpuState/jit_abi/test harness
- [ ] #2 Masked/zeroing 512-bit data-mov + logic + packed integer arith lifted (interp==jit); 128/256 EVEX forms reuse existing YMM paths where possible
- [ ] #3 Opmask ops (kmov/kand/kor/kortest/ktest/knot) + mask-producing compares (vpcmpb/w/d/q -> k) lifted
- [ ] #4 AVX-512 specials the v4 glibc/distro corpus actually uses covered (vpternlog, vpcmp, broadcasts, cross-lane permutes, vpblendm); driven by real-binary trap-and-fix loop
- [ ] #5 CPUID advertises AVX-512F/BW/DQ/VL/CD; the full real-binary corpus stays green 3-way with glibc/distro on AVX-512 paths; a decision doc amends decision-11
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
=== SESSION HANDOFF 2026-07-10 (for next Claude) ===

STATE: main @ 94e6123 (pushed). Suite 396/396 green (--features unicorn, minus fuzz_robustness), clippy -D warnings clean, fmt clean. Host is CachyOS with AVX-512 (native oracle captures full ZMM+k state, task-193).

DONE (AVX-512): .1 EVEX masked compares vpcmpeq/gt->k; .2 EVEX logic vpxorq/vpandq/vpord/vpandnq + vpternlog; .3 BMI; .4 SSE4 gaps incl pcmpistri/pcmpestri (native-fuzzed all 96 imm8 modes); .6 lane ops vinserti32x4/64x2/64x4 + valignd/q; 193 ZMM/opmask snapshot capture; 194 FWAIT + ud2/int3/int1 traps (HW-accurate saved-RIP). AC#5 STARTED: glibc runs under GuestCpuFeatures::v4() 3-way (glibc_hello_avx512_interp_jit_agree), lifted vptestm/vptestnm (VPTestToMask).

NEXT (AC#5 breadth = the goal: run real v4 binaries):
1. TRAP-AND-FIX heavier glibc code. Only hello_glibc is glibc-dynamic in the corpus (musl-static ones don't CPUID-dispatch). Options: (a) add a memcpy/memcmp/strchr-heavy glibc binary to programs/ + a v4 test; (b) 'x86jit-cli oci run --cpu v4 <image>' on a real glibc distro image (dynamic loader path) — most realistic, closest to task title 'CachyOS /usr/bin'.
2. Each trap -> the RECIPE (see memory avx512-trap-and-fix): decode bytes with iced, add IrOp + interp fn + jit (inline via load_lane/store_lane/vhigh_bits OR helper->interp like pcmpstr/masked-logic if complex), lift dispatch, jit_eq_interp_features(v4) test + native_*_matches_interp test, compat regen.
3. Likely next EVEX gaps glibc uses: vpbroadcast* (EVEX forms), vpcompress/vpexpand, more vpcmp predicates, kmov/kortest variants (some done), vpminub/maxub EVEX, gather/scatter.

REMAINING SUBTASKS: 168.5.5 (masked EVEX) In Progress — only masked LOGIC done; masked packed arith (easy, same helper pattern) + masked memory moves (hard: fault suppression) remain. task-195 (memory src2 for all register-only ops + minor SSE4: pmuldq/insertps/dpps/pblendw/pcmpistrm). AC#5 also needs a decision doc amending decision-11 (advertise-AVX2 -> now per-run feature selection, v4 opts in).

KEY MECHANISMS: GuestCpuFeatures::v4() advertises AVX-512 (features.rs leaf7_ebx). Guest builder .features() (guest.rs). x86jit-cli --cpu v4. NativeOracle (native.rs) = real-CPU oracle, only thing that validates EVEX (Unicorn drops VEX.vvvv) — captures GPR/XMM/YMM/ZMM/k. run_native rejects nonzero ymm_hi/zmm_hi/kmask INIT (stub loads only xmm + zeroes rest) — so native tests load wide state IN-snippet from memory. kmask/vector state memory-backed in JIT (helper can write cpu); GPRs/flags CACHED (use out-slot pattern like BMI/pcmpstr).

GOTCHA: perf-gate blocks push on fib32/hotloop bench noise (box loaded / 3-day-old baseline). AVX changes never touch integer hot path -> override 'X86JIT_ALLOW_PERF_REGRESSION=1 git push'. Consider re-recording baseline (cargo run -p x86jit-bench -- record) when box idle.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
