---
id: TASK-167
title: OCI registry pull (Route A) — skopeo/crane to oci-layout + reader + CI wiring
status: To Do
assignee: []
created_date: '2026-07-08 14:10'
labels:
  - 'crate:oci'
  - 'goal:feature'
dependencies: []
ordinal: 176000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Implement decision-10: pull OCI test images straight from a registry (no docker daemon, no committed tar, no hand-built ELF) and run them three ways. Route A = shell out to a static puller (skopeo/crane/oras) fetching a DIGEST-PINNED image to an oci-layout dir, then add an oci-layout reader in x86jit-oci alongside the existing docker-save load_image so all downstream (rootfs/config/argv-override/run) is reused. Digest-pinned (never :latest); mirror off Docker Hub (ghcr/quay/ECR) to dodge anon rate limits; cache blobs/rootfs by digest; gate to no-op when the puller/network is absent (like ubuntu.rs). See backlog/decisions/decision-10.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 x86jit-oci reads an oci-layout dir (from skopeo/crane) into the same rootfs+ImageConfig as load_image, resolving the amd64 manifest from a multi-arch index
- [ ] #2 A gated test pulls a digest-pinned minimal image (e.g. busybox from a non-Hub mirror) and runs a swapped-entrypoint command three ways (native/interp/JIT); no-ops with a note when the puller/network is unavailable
- [ ] #3 CI installs the puller + caches by digest; the pull is digest-pinned and hits the registry at most once per digest bump
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
