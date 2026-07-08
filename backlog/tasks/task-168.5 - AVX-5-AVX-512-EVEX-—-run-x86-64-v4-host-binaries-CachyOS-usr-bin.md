---
id: TASK-168.5
title: 'AVX-5: AVX-512/EVEX — run x86-64-v4 host binaries (CachyOS /usr/bin)'
status: In Progress
assignee: []
created_date: '2026-07-08 17:53'
updated_date: '2026-07-08 18:39'
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
EXPERIMENT (throwaway, reverted): advertised full v2/v3/v4 CPUID + XCR0=0xE7 -> /usr/bin/true clears glibc CPU-level check, then traps on EVEX vpxorq. Scanned glibc AVX-512 IFUNC surface (objdump) — the concrete 'what we miss for v4' gap list, by frequency: (1) EVEX vpcmpeqb/eqd/gtb/neqb/neqd -> k [~2000 uses, #1 — dedicated-opcode masked compares; my vpcmp{b,w,d,q} imm-form lift does NOT cover these named-opcode forms]; (2) EVEX logic vpxorq/vpandq/vpord/vpandnq + vpternlogd[40] [EVEX routing + vpternlog needs a truth-table op]; (3) pcmpistri[204]/pcmpestri [SSE4.2 — from advertising v2, complex, decision-2]; (4) BMI1/2: shrx[66]/blsmsk[56]/bzhi[36]/sarx[23]/shlx[21]/andn + bextr/blsr/blsi/pdep/pext/mulx/rorx [from advertising v3]; (5) lzcnt[27]/tzcnt/movbe[16] scalar v3; (6) masked/zeroing data ops [303 {k} sites — merge/zero subsystem]; (7) EVEX lane ops vinserti64x2/x4, valignq. PLUS the CPUID-advertise gate itself (decision, reopens decision-2). Confirmed unlifted: Lzcnt/Tzcnt/Bzhi/Shrx/Sarx/Shlx/Blsmsk/Movbe/Vpternlogd/Pcmpistri. Order to tackle: EVEX vpcmpeq*->k first (biggest), then EVEX logic+vpternlog, then BMI, then pcmpistri, then masked data ops, then advertise+corpus-loop.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
