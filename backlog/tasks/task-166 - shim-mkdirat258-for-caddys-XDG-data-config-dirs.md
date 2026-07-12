---
id: TASK-166
title: 'shim: mkdirat(258) for caddy''s XDG data/config dirs'
status: Done
assignee: []
created_date: '2026-07-08 13:41'
updated_date: '2026-07-12 10:38'
labels:
  - go-caddy
  - 'crate:linux'
  - 'goal:feature'
dependencies:
  - TASK-153
ordinal: 175000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Real caddy file-server (task-153) calls mkdirat(258) at startup to create its XDG_DATA_HOME/XDG_CONFIG_HOME dirs; it falls through to the shim's -ENOSYS default. Non-fatal today — caddy serves index.html fine without the data dir — but for correct behavior implement mkdirat: honor an allow_write_dir target (mkdir under the rootfs), EROFS/EACCES otherwise, mirroring the existing openat/unlinkat dir handling. Refs: gap log line 'unhandled syscall 258 -> -ENOSYS' when running caddy file-server.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 mkdirat(258) creates a directory under an allow_write_dir target (or returns a plausible errno); caddy's XDG dir creation no longer logs an unhandled-syscall gap
- [ ] #2 oci test: caddy (or a minimal guest) mkdirat-creates nested XDG dirs and a follow-up openat succeeds
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE via task-215 caddy work (commit 037cdb2): mkdir(83)+mkdirat(258) added to shim.rs, gated to writable passthrough dirs (resolve_host_write). caddy creates its XDG data/config + local-CA dirs successfully.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
