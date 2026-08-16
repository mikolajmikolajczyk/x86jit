---
id: TASK-353
title: 'core: two unproven ordering and mode assumptions'
status: To Do
assignee: []
created_date: '2026-08-15 13:28'
labels:
  - code-review
dependencies: []
priority: medium
ordinal: 389000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Two things this review argued but could not confirm by execution on an x86 host. Recorded so they are not rediscovered as surprises.

A. The 'a watch installed mid-run is seen by the next store' claim has no ordering behind it. `watch()` does watch[w].fetch_or(bit, Relaxed) then count.fetch_add(1, Relaxed) (memory.rs:444-459); generated code does a plain load of count then a plain load of the bitmap word (codegen/mod.rs:2663-2700), both MemFlags::trusted(), no acquire. Nothing orders the bit-set against the count-increment, so on AArch64 a store may observe count != 0 and a bitmap word without the bit. Unobservable on a TSO host, and no ARM host was available here.

The same shape exists for mark_code (memory.rs:687-697: code_page bit Relaxed, then code_range widened Relaxed) against the compiled store's code_range-then-bitmap read. For that one the exposure is limited to cross-vcpu code modification, which SDM Vol 3A §11.1.3 already calls model-specific absent the guest's serializing handshake, as cross_modifying.rs documents. The watch case has no such cover: it is the documented mid-run cross-thread registration path.

The store side (bit before watermark/count) is at least the correct direction in both cases.

B. Nothing prevents CpuMode::Real16 reaching the JIT. A Real16 block of only mode-neutral ops compiles fine; then Vcpu::fast_get/fast_put key on cpu.rip (the 16-bit IP) while resolve keys on fetch.pa, so two segments with the same IP alias to one fast-cache slot, and resolve's tier-up span uses ir.guest_start (the IP) rather than the physical PA. A block that does contain a real-mode op hits unreachable! in codegen/mod.rs:1783 -- a panic. The design says Real16 never reaches the JIT; nothing enforces it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The watch bitmap/count pair is ordered so a store cannot see the count without the bit, verified on the aarch64 lane
- [ ] #2 Real16 with a JIT backend is either refused at the seam or made correct; the unreachable! is not a reachable guest path
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
