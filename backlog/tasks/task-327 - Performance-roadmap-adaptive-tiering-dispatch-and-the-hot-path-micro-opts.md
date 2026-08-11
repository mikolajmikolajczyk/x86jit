---
id: TASK-327
title: 'Performance roadmap: adaptive tiering, dispatch, and the hot-path micro-opts'
status: To Do
assignee: []
created_date: '2026-08-11 11:03'
labels: []
dependencies: []
priority: low
ordinal: 363000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Eight perf tasks collapsed into one, because they share the finding that makes all of them provisional.

task-216 measured where the time goes: ~30 host cycles per guest instruction, attributed to flag materialization at block exit, not to the mid-end. And the harder lesson, recorded from the embedder side (task-217, doc-28): four micro-optimisations that measured well locally moved the real workload by zero. So each item below needs a workload that demonstrates the gain before it is worth its complexity — that gate is the point of merging them.

**Adaptive tiering** (was 108, 109, 110, 111, 213): a dedicated region-compile worker so heavy region compiles do not clog single-block tier-up; tier thresholds that scale with compile-queue and code-cache pressure; a hotloop-length sweep that validates the tier chosen is the right one; compiled backedge counters for baseline→region OSR; and exposing the tiering primitives so an embedder can define its own policy.

**Dispatch** (was 212, 214): the per-site indirect-branch cache is monomorphic, so two or more targets cost 2.6x; and chained block transfers still round-trip through the Rust dispatcher — task-62 delivered caching, not stitching.

**Hot path** (was 172): RAM bounds-check elision, guest-register residency, a lazy-flags audit. Note lazy flags themselves are in deferred.md, with task-216's measurement as the reason to revisit.

Do not start any of these without a workload that would show the improvement — that is what the last round of perf work established.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Each item is either delivered with a measured gain on a real workload, or recorded as not worth it with the measurement that says so
- [ ] #2 The indirect-branch cache handles 2+ targets without the 2.6x cliff, or the cliff is shown not to matter
- [ ] #3 Chained transfers stop round-tripping through the dispatcher, measured end to end
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
