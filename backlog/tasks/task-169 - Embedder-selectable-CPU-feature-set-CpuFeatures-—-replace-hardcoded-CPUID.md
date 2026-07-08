---
id: TASK-169
title: Embedder-selectable CPU feature set (CpuFeatures) — replace hardcoded CPUID
status: In Progress
assignee: []
created_date: '2026-07-08 18:54'
updated_date: '2026-07-08 18:55'
labels:
  - m8-simd
  - 'crate:core'
  - 'goal:feature'
  - 'goal:api'
dependencies: []
ordinal: 183000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Replace the hardcoded cpuid_run + baked xgetbv with an embedder-configurable CpuFeatures value chosen per-run (presets baseline/v2/v3/v4 + with/without toggles). Default = today's exact advertised set (zero regression). Dissolves the risky global AVX-512 advertise gate into a per-run parameter; correct library API (embedder declares guest CPU like qemu -cpu). Supersedes the global model of decision-2/decision-11. See plan rustling-beaming-moonbeam.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 CpuFeatures type in x86jit-core: presets baseline/v2/v3/v4/stable + with/without/has + CPUID leaf projections + xcr0(); Default=stable (current set)
- [ ] #2 cpuid_run + xgetbv (now runtime IrOp::Xgetbv) read cpu.features; both interp and JIT backends; Vm::set_cpu_features setter mirrors set_tier_up_after
- [ ] #3 Harness (VectorInput/TestVector serde-default, jit_eq_interp_features, guest builder) + runners (x86jit-cli/run --cpu flag) can pick a feature set per run/test
- [ ] #4 compat: Gen::V4 added; default-preset advertise-subset-of-lifted invariant intact; coverage.json regenerated
- [ ] #5 Full non-fuzz suite green with zero behavior diff (Default=today); jit==interp on a v4 AVX-512 snippet; x86jit-cli --cpu v4 /usr/bin/true clears glibc x86-64-v2 level check
- [ ] #6 decision-12 recorded (features embedder-configured, supersedes global model of decision-2/11)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
