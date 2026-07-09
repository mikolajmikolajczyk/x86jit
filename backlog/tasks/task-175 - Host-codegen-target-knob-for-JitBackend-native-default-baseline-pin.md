---
id: TASK-175
title: Host codegen target knob for JitBackend (native default + baseline pin)
status: Done
assignee: []
created_date: '2026-07-08 20:38'
updated_date: '2026-07-09 08:49'
labels:
  - 'crate:cranelift'
  - 'goal:feature'
  - 'goal:api'
  - seq-3
dependencies: []
ordinal: 199000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Separate axis from GuestCpuFeatures: which HOST instructions Cranelift may emit to implement the IR (performance / portability), independent of the guest ISA. Today x86jit-cranelift/src/lib.rs:284 hardwires cranelift_native::builder() -> all host features (so it ALREADY uses host AVX2 etc to optimize guest code regardless of guest ISA — just not configurable). Expose a knob: JitBackend target config, native by default, with an optional baseline pin (e.g. x86-64-v2/v3) so JIT output is deterministic/portable across hosts and AOT-cacheable, or to disable a flaky host feature. Maps to cranelift settings/ISA flags. Does NOT read GuestCpuFeatures. Note: cranelift host AVX-512 codegen is limited/opt-in; AVX2 is the practical win.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 JitBackend accepts a host-target config (enum native | baseline(level) or explicit ISA flags); default = native (current behavior)
- [ ] #2 Verified: baseline-pinned JIT emits no host insns above the pin; native unchanged; suite green
- [ ] #3 Doc: host codegen target is orthogonal to GuestCpuFeatures (guest CPUID)
- [ ] #4 Knob lives on JitBackend construction, NOT VmConfig — interpreter has no host-codegen target (it is plain Rust compiled by rustc); putting it on the shared config would wrongly imply it affects interp
- [ ] #5 Guest-invisible: only bit-identical host instruction selection; MUST NOT enable semantics-changing opts (FMA contraction, fast-math) or interp==JIT differential testing breaks — interpreter stays the reference oracle
- [ ] #6 ARM is the primary host — the target levels there are ARM feature tiers (NEON baseline / SVE), not just x86 v2/v3/v4; the knob generalizes across host arches. x86-host is mainly differential/native-oracle, so x86 host-target pinning is secondary
<!-- AC:END -->





## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
