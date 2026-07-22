---
id: TASK-279
title: >-
  embedder-defined tier-up policy: expose the tiering primitives so an embedder
  can build its own
status: To Do
assignee: []
created_date: '2026-07-22 08:11'
labels:
  - jit
  - dispatch
  - api
  - embedder
dependencies: []
ordinal: 309000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Today the tier-up policy is a fixed shape baked into the core dispatcher: `Vm::set_tier_up_after(Some(n))` counts executions of an interpreted block and compiles it at n, `set_tier_up_background` moves that compile to the backend worker, and region formation has its own separate threshold (task-156's adaptive T2). An embedder can turn those knobs but cannot replace the decision.

Real embedders want different policies. A game emulator knows its frame boundary and could tier aggressively during a load screen and not at all mid-frame; it knows which guest addresses are engine code versus one-shot init; it may want to demote or re-tier after a phase change. None of that is expressible by a single execution counter.

The pieces are already separable — the dispatcher counts executions, `Backend::materialize` / `tier_up_async` compile, `TranslationCache::upgrade` publishes, `invalidate_overlapping` drops, and `Backend::set_tiering` (task-276) already carries one policy fact across the seam. What is missing is a seam where the EMBEDDER, not the core, answers 'should this block be compiled now, and how'.

Sketch (maintainer's call): a trait the embedder implements, consulted where the counter is checked today — given the block key, its execution count, and cheap context (is it in a region candidate, current epoch), it returns compile-now / stay-interpreted / compile-as-region. Default impl reproduces today's `tier_up_after` behaviour exactly, so nothing changes for existing callers.

Watch out for: (1) it sits on the hot dispatch path, so the default must stay as cheap as the current integer compare — a virtual call per dispatch would be a regression; (2) the JIT's opt_level is baked into the ISA at first compile (task-276), so a policy that wants different codegen per block needs the two-module problem solved first; (3) whatever is exposed becomes API the core must keep working across the SoftMmu/AOT milestones, so keep the surface minimal.

Raised by the unemups4 embedder while reviewing task-276: 'powinno się dać też zbudować customowy mechanizm tierowania bazując na naszych klockach'.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 An embedder can supply its own tier-up decision without forking the dispatcher, and a supplied policy demonstrably changes when/how blocks are compiled
- [ ] #2 The default policy reproduces today's tier_up_after behaviour exactly — existing embedders and every test are unaffected without code changes
- [ ] #3 No measurable regression on the hot dispatch path for the default policy (perf gate green, and a targeted before/after on the dispatch-micro bench)
- [ ] #4 The seam is documented in spec.md with the boundary stated: what the core decides vs what the embedder decides
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
