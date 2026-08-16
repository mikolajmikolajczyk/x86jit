---
id: TASK-352
title: >-
  core: guest self-modifying code grows host memory without bound and ends in a
  panic, not an Exit
status: To Do
assignee: []
created_date: '2026-08-15 13:28'
labels:
  - code-review
dependencies: []
priority: high
ordinal: 388000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Nothing reclaims a dropped translation: the cranelift arena never frees, ibtc_descriptors is documented 'never freed before Vm drop', and CodeMap is append-only. A guest that patches a callee in a loop -- what a guest-level JIT does to an inline cache, the exact shape smc.rs already tests once -- forces one fresh compile per patch.

Probe, single-threaded, no race, JIT backend; the guest result was correct in every run:

    2000 guest patches -> RSS + 19372 KiB,  2011 exec mappings ( 13944 KiB); ibtc descriptors  2000
    8000 guest patches -> RSS + 66408 KiB, 10015 exec mappings ( 45960 KiB); ibtc descriptors  8000
   32000 guest patches -> RSS +265380 KiB, 42019 exec mappings (173976 KiB); ibtc descriptors 32000

Strictly linear: ~8 KiB RSS, ~1.3 executable VMAs and exactly one leaked IBTC descriptor per guest patch. The IBTC cap does not bound it -- IBTC_MEGAMORPHIC_CAP is enforced through Vcpu::ibtc_refills, which fast_clear resets on every epoch change, and an SMC drop IS an epoch change.

A second probe (a vcpu running a chained call loop while another thread rewrote the callee page) reached the end in ~48 s:

    panicked at x86jit-cranelift/src/lib.rs:2709:
    finalize: Backend(unable to make memory readable+executable
    Caused by: System call failed: Cannot allocate memory (os error 12))

The host process aborts. Not an Exit the embedder can handle, and reached sooner than RAM exhaustion on a default host: vm.max_map_count is 65530 by default (this host is at 1048576). CodeMap::register has its own hard stop, assert!(ci < MAX_CHUNKS, "CodeMap capacity exhausted") at 4.19M compiles -- also a panic.

Separately, CodeMap::lookup (codemap.rs:118) is a linear scan over every compile ever made, and it is called from a signal handler: at 32000 patches that is a 42000-entry scan per fault.

Not in README's Known gaps, and not in deferred.md.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Dropped translations release their code memory and their IBTC descriptors, or the growth is bounded and the bound is documented
- [ ] #2 Exhaustion surfaces as an Exit the embedder can handle, not a panic
- [ ] #3 CodeMap::lookup is not a linear scan over every compile ever made
- [ ] #4 Until the reclaim lands, README states the growth and its terminal behaviour
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
