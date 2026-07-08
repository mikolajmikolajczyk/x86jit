---
id: TASK-174
title: >-
  Rename CpuFeatures -> GuestCpuFeatures (disambiguate guest ISA from host
  codegen target)
status: In Progress
assignee: []
created_date: '2026-07-08 20:38'
updated_date: '2026-07-08 21:10'
labels:
  - 'crate:core'
  - 'goal:refactor'
  - 'goal:api'
  - seq-1
dependencies: []
ordinal: 198000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The CpuFeatures API (task-169) models ONE axis: what CPUID/xgetbv advertise to the GUEST (which instructions the guest program uses — correctness/compat). The name reads generic and collides conceptually with the separate host-codegen-target axis (task for that filed separately). Rename for clarity: CpuFeatures -> GuestCpuFeatures, Vm::set_cpu_features -> set_guest_cpu_features, cpu_features() -> guest_cpu_features(), run_config_argv_stdin_features + x86jit-cli keep working. Feature enum + presets keep names. Pure rename + doc: state clearly it is guest-facing and does NOT affect what host instructions Cranelift emits. Small, mechanical.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 CpuFeatures->GuestCpuFeatures across core/run/cli/tests; setter/getter renamed; docs state guest-vs-host-codegen distinction
- [ ] #2 No behavior change; suite + compat green
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
