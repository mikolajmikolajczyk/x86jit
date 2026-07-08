---
id: decision-10
title: >-
  Pull OCI test images from a registry (Route A: skopeo/crane, digest-pinned)
  instead of committing docker-save tars
date: '2026-07-08 14:08'
status: proposed
---

**Deciders:** Mikołaj Mikołajczyk

## Context

Test surface today comes from two fixture sources, both with friction:

- **Hand-built guest ELFs** (`x86jit-tests/programs/*.elf`) — busybox, lua,
  sqlite, the `*_go.elf` Go stand-ins, and now the git-ignored `caddy.elf`
  (task-153). Building each is tedious (toolchains, static linking, stripping),
  and the big ones (caddy ~52 MiB) can't be committed, so their tests are gated
  on a locally-built file being present (they no-op in CI).
- **`docker save` tarballs** (`x86jit-oci/fixtures/*.tar`) — `x86jit-oci`'s
  `load_image` already turns a `docker save` tar into a rootfs + `ImageConfig`
  (Env/Cmd/Entrypoint/WorkingDir), applies gzip layers in order, and the runner
  already overrides argv (e.g. `ubuntu.rs` runs `dash -c '…'`). Small pinned
  images are committed; large/moving ones (ubuntu) are git-ignored and require a
  local `docker save`, so those tests also no-op in CI.

The whole downstream — rootfs materialization, config parsing, argv override,
run three ways (native/interp/JIT) — **already works**. The only missing piece
is **acquiring** an image without a local Docker daemon and without committing a
tar. A real OCI image is a huge, ready-made test surface (every binary in
alpine/busybox/ubuntu/…); "pull + swap entrypoint + run" would let us test broad
ISA/syscall coverage without building a single ELF.

Fetching an image needs no container runtime — the OCI Distribution API is plain
HTTPS: (1) get a pull token from the registry auth endpoint; (2) GET the
manifest, resolving a multi-arch **image index → the amd64 manifest** (the guest
is x86-64 regardless of host); (3) GET the config blob + each layer blob
(`/v2/<repo>/blobs/<digest>`, tar.gz); (4) extract layers in order → rootfs,
parse config. That is exactly the rootfs `load_image` already produces, just
sourced from a registry instead of a `docker save` tar.

## Decision

Add a **registry-pull path** to the OCI test infrastructure, **Route A: shell
out to a static puller** (`skopeo` — or `crane`/`oras`) to fetch an image to an
**oci-layout directory**, then add a small oci-layout reader alongside the
existing `docker save` parser in `x86jit-oci` so everything downstream is reused.

Constraints, all mandatory:

- **Digest-pinned, never `:latest`.** Reference images by `repo@sha256:…` so a
  drifting tag can't make a test flaky or non-reproducible.
- **Mirror away from Docker Hub.** Anonymous Docker Hub is rate-limited (~100
  pulls / 6 h / shared IP) — CI runners share IPs and would flake. Pull from
  `ghcr.io` / `quay.io` / public ECR (higher/no anon limits), or a repo-owned
  mirror.
- **Cache by digest.** `actions/cache` keyed on the image digest holds the
  pulled blobs / extracted rootfs so reruns are offline and fast, and the
  registry is hit at most once per digest bump.
- **Gate like the existing fixtures.** If the puller or network is unavailable
  (e.g. a fork's CI with no egress), the test no-ops with a note rather than
  failing — same policy as `ubuntu.rs`.

Route B (a native Rust registry client: `reqwest` + token auth + manifest-list
resolution + blob fetch/verify + tar/flate2 extraction) is deferred. It buys
zero external-tool dependency at the cost of owning every distribution-spec edge
(schema2 vs OCI media types, manifest lists, digest verification, auth refresh).
Revisit only if the `skopeo` dependency becomes a burden.

## Consequences

- **Positive:** broad, cheap test surface (any registry image) with no ELF
  building and no committed tars; reuses all of `x86jit-oci`'s
  rootfs/config/run machinery; digest-pinning makes runs reproducible.
- **Cost:** CI gains a dependency on a static puller binary + network egress +
  a cache step; a new oci-layout reader in `x86jit-oci` (small).
- **Risks / mitigations:** registry rate limits → mirror + digest cache; tag
  drift → digest pins; multi-arch images → always resolve the amd64 manifest;
  large images (python ~1 GB) → prefer minimal variants and cache aggressively.
- **Reversible:** the puller is additive; the committed-tar and hand-built-ELF
  paths stay. If Route A proves fragile we fall back to committed fixtures or
  graduate to Route B without touching downstream code.

## Alternatives considered

- **Keep building ELFs / committing tars (status quo).** Rejected as the default
  for broad coverage: tedious, and big fixtures can't be committed so their
  tests never run in CI.
- **Route B — native Rust puller now.** Deferred: more code and every
  distribution-spec corner to own, for a purity (no external tool) we don't yet
  need.
- **Run a real Docker daemon in CI.** Rejected: heavy, needs privileged runners,
  and contradicts the "no container runtime, just layers + config" model
  `x86jit-oci` is built on.
