---
id: decision-13
title: >-
  AVX-512 write-masking via a shared blend helper + per-op MaskSpec (not per-op
  masked IR variants)
date: '2026-07-08 21:49'
status: accepted
---

**Deciders:** Mikołaj Mikołajczyk

## Context

AVX-512 EVEX ops carry a write-mask (k1–k7) and a zeroing bit (`{k}{z}`): for each
`elem`-wide lane `i`, `dst[i] = k[i] ? result[i] : (z ? 0 : dst[i])` (merge vs
zero). This applies to nearly every data-movement / arithmetic / logic op. glibc's
v4 routines use ~303 masked sites. Masking is a **modifier on every op**, not one
op — the risk (task-170) is that adding a masked IR variant per op doubles the
already-68 vector variants. Today `evex_is_masked()` → `Unsupported`, so support is
**additive**: no existing test exercises it, nothing can regress.

## Decision

Masking is **one shared blend helper**, not per-op masked variants.

- **`CpuState::write_masked(dst, newval: [u128;4], k, elem, zeroing, bytes)`** — the
  single place the merge/zero rule lives. Reads the old dst (via `vec_lanes`, unless
  zeroing), blends `newval` per `kmask[k]` bit at `elem` granularity across `bytes`,
  commits via `set_vec` (task-170.3). Cranelift gets the mirror emitter.
- A maskable op computes its **full unmasked `newval`** as it already does, then
  routes the write through `write_masked` instead of `set_vec` when a mask is
  present (`set_vec` = the k0/unmasked fast path).
- The op carries a small **`MaskSpec { k: u8, zeroing: bool }`** (Option; `None` =
  unmasked) rather than a bespoke masked opcode. `elem`/`bytes` the op already knows.

Merge-masking needs the pre-value of dst; because the op computes `newval` into a
value (lanes) before committing — not in-place into dst — the old dst is still
intact at blend time. No vector scratch register or save/restore op is needed.

k0 as a write-mask means "no masking" (hardware convention): lift maps k0 → `None`,
so masked and unmasked share the same op with the mask spec absent.

## Consequences

- One helper + one MaskSpec field per maskable op — the vector op count does **not**
  balloon. Adding a new masked op = compute `newval` + call `write_masked`.
- Green-safe / additive: existing (unmasked) paths keep calling `set_vec`; no
  behavior change until an op is actually masked.
- Compares-to-opmask (`vpcmp → k`, task-168.5) are unaffected: their write-mask
  ANDs into the *k result*, already handled in `VPCmpToMask`. This decision covers
  masked writes to *vector* destinations.
- Proof of the abstraction lands with the first masked op (masked `vmovdqu32/64`),
  jit==interp tested under `GuestCpuFeatures::v4()`.

## Alternatives considered

- **Per-op masked IR variants** (`VMaskMov`, `VMaskPackedBin{z}`, …) — doubles the
  68 vector variants and the interp+cranelift arms. Rejected (the whole point of
  task-170).
- **A post-op `VApplyMask` blending a scratch vector** — needs a spare vector
  register or a save/restore op (no vector temps exist in the IR). Rejected: the
  compute-into-value-then-commit shape already preserves old dst, so it's unneeded.

## Trigger to revisit

If an op genuinely can't compute its full result into a value before committing
(e.g. an in-place gather/scatter), or if embedded-broadcast (`{1toN}`) or embedded
rounding needs to compose with masking in a way the flat MaskSpec can't express.
