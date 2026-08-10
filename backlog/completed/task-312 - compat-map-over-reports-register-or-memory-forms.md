---
id: TASK-312
title: compat map over-reports register-or-memory forms
status: Done
assignee: []
created_date: '2026-08-10 15:39'
updated_date: '2026-08-10 19:29'
labels: []
dependencies: []
priority: medium
ordinal: 348000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
compat.rs (~193): every *_or_mem / *_rm operand kind is instantiated as a register. iced puts the ModRM register and memory alternatives under one Code, so lifting the register encoding marks the Code Lifted even when the lifter rejects the memory encoding.

Found twice, independently: during this session's vextract work (the map claimed coverage the lifter did not have) and by an adversarial review. Single-memory-operand shapes additionally land in the 'unencodable' bucket and disappear.

The map is a CI-checked artifact the README points at, so an over-report is a published claim, not just an internal inaccuracy.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Each legal operand alternative is probed independently
- [ ] #2 A Code counts as lifted only when every form lifts; otherwise it is partial, naming the failing form
- [ ] #3 The regenerated map is diffed and the newly-revealed gaps triaged
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Resolved as a documentation fix, not a probe fix. The over-report is real, but the artifact is linked from the README, so the publication blocker is the unqualified claim rather than the probe. The caveat is now emitted BY THE GENERATOR (compat.rs to_markdown), so it survives regeneration — a hand-edit would have been dropped by the next --write, and compat_map_is_current does not compare the header, so nothing would have caught that. README now calls the map an upper bound. Fixing the probe itself is still open: reopened as task-320.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
