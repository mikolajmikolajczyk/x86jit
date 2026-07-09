---
id: TASK-180
title: 'CR — core-support dedup: vm.rs ctor defaults, features.rs tier composition'
status: Done
assignee: []
created_date: '2026-07-09 09:56'
updated_date: '2026-07-09 10:29'
labels:
  - CR
dependencies: []
ordinal: 204000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Code-quality consolidation. No behavior change (Default preset unchanged). (N) vm.rs with_backend/with_backend_host_ram repeat every field default (cache/backend/consistency/tier_up_* /features) differing only in mem: -> shared private from_mem(mem,backend,consistency) ctor. (O) features.rs v2/v3 re-list every lower-tier feature verbatim; only v4 uses compositional Self::v3().0 | delta -> make v2/v3 compose the same way (needs const-friendly bits helper). Verify: build + nextest + a features unit test still green (tier deltas).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 vm ctors delegate to a shared from_mem,v2/v3 compose from lower tier + delta,Default preset bit-identical to before,full suite green + clippy clean
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
