---
id: TASK-337
title: 'interp: vector MMIO still hangs outside the vpart path'
status: To Do
assignee: []
created_date: '2026-08-15 13:24'
labels:
  - m8-simd
dependencies: []
priority: high
ordinal: 373000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
task-332 is Done with an acceptance criterion saying a vector MMIO access never loops. That holds only for accesses routed through `vpart`/`vpart_store`. Handlers that call `Memory::read`/`write` or `StrMem::sload`/`sstore` directly never consult `mmio_parts`, so each retry re-consumes one answer and the access can never converge.

Probe, RegionKind::Trap region, embedder answers every exit:

    vfmadd213ps xmm0,xmm1,[rax]   LOOPS FOREVER: [(4000,8) x10]
    vmovdqu32   xmm0{k1},[rax]    LOOPS FOREVER: [(4000,4) x10]
    vpermt2d    xmm0,xmm1,[rax]   LOOPS FOREVER: [(4000,8) x10]
    vbroadcasti32x4 ymm0,[rax]    LOOPS FOREVER: [(4000,8) x10]
    vpmovqd     [rax],xmm0        LOOPS FOREVER: [(4000,4) x10]
    vbroadcastss xmm0,[rax]       LOOPS FOREVER: [(4000,4) x6]
    movdqu xmm0,[rax] (control)   CONVERGED after 2 exits: [(4000,8),(4008,8)]

Guest sees a hang; the embedder cannot work around it, because answering the exit changes nothing.

Sites: 15 `mem.sload/sstore` call sites in interp/mod.rs, reached from 10 `*_run` sites in interp/vector.rs (masked_load_run, masked_store_run, narrow_store_run, fma_mem_run, broadcast_lane_mem_run, permute2_run, vperm1_run, gf2p8_mem_run), plus the direct `mem.read` at interp/vector.rs:2265 (exec_v_broadcast_m).

fault_atomicity.rs's MMIO tests only exercise movdqu, which goes through vpart and converges — which is why a Done task reads as covering this.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every vector memory path either routes through vpart/vpart_store or is otherwise resumable across retries
- [ ] #2 A loop test covers at least one handler from each of the eight *_run families, not just movdqu
- [ ] #3 task-332's record is corrected to say what it actually closed
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
