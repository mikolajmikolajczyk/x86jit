---
id: TASK-168.4
title: 'AVX-4: CPUID advertise AVX/AVX2 + amend decision-2'
status: Done
assignee: []
created_date: '2026-07-08 15:21'
updated_date: '2026-07-08 17:43'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
dependencies:
  - TASK-168
parent_task_id: TASK-168
ordinal: 181000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Once 128+256 lifting is solid, flip cpuid_run to advertise AVX (+AVX2/BMI as covered) so glibc IFUNC selects the AVX paths, and write a decision amending decision-2 (which currently masks SSE4+ to force SSSE3). Gate LAST — advertising before lifting is solid exposes unrunnable paths.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 CPUID advertises AVX/AVX2; the differential corpus (busybox/alpine/glibc/sqlite/lua/cpython + native oracle) stays green with glibc now selecting AVX string routines; a decision doc records the change
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE. Lifted xgetbv (XCR0=0x7) and flipped cpuid_run: leaf1 ECX += XSAVE|OSXSAVE|AVX, leaf7 EBX += AVX2; SSE4 stays off (decision-2 intact, AVX2 routines are VEX). Advertisement exposed exactly one gap — vptest (VEX 0F38.17, 128+256), used by Go AVX2 memmove/memclr; lifted in interp+cranelift (VPtest IR: ZF=(b&a==0), CF=(b&!a==0)). Full LOCAL real-binary corpus green 3-way (native==interp==jit) with glibc/Go on AVX2 paths: busybox/gzip/djpeg (static glibc), python3/sqlite/lua/dynamic/musl/pthreads (dynamic glibc/musl), Go hello/net/http/caddy. New unit tests avx_vptest_matches_interp + avx2_cross_lane_permutes_match_interp. decision-11 records it. CI caveat: OCI/registry corpus SKIPs in CI (no network), AVX2 on those verified locally only.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
