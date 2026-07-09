---
id: TASK-181
title: 'CR — x86jit-run: EngineConfig struct (tier-up/superblocks/host-target off env)'
status: Done
assignee: []
created_date: '2026-07-09 11:00'
updated_date: '2026-07-09 11:47'
labels:
  - CR
dependencies: []
ordinal: 205000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Consolidate the JIT tuning knobs in x86jit-run behind an explicit config struct, and move env-var parsing to the binary boundary.

## Problem
Two smells today:
1. Env vars read INSIDE library functions: `EngineKind::backend()` reads `X86JIT_BG_REGION` + `X86JIT_HOST_BASELINE`; `load_process` reads `X86JIT_BG_TIER`. A library consulting process env is untestable, non-obvious, and not programmatically overridable.
2. Config split across two places: `backend()` decides superblocks + host_target; `load_process` decides tier-up mode. No single description of "how the JIT runs".

`EngineKind` must stay a clean 2-variant enum (Interpreter | Jit) — the differential path iterates [Interpreter, Jit] (`--backend both`, tests). So the tuning belongs in a SEPARATE struct, not as flags on EngineKind.

## Shape
```rust
pub enum TierUp { Off, Inline, Background }
pub struct EngineConfig {
    pub kind: EngineKind,
    pub tier_up: TierUp,
    pub superblocks: bool,
    pub host_target: HostTarget,
}
impl EngineConfig {
    pub fn from_env(kind: EngineKind) -> Self { /* reads X86JIT_* — escape hatch, explicit */ }
    fn backend(&self) -> Box<dyn Backend> { /* pure, no env */ }
}
impl Default for EngineConfig { /* Jit, Inline, false, Native = today */ }
```

## Migration (behavior-preserving, no forced call-site edits)
- `impl From<EngineKind> for EngineConfig` = `from_env(kind)`. The run_* family takes `impl Into<EngineConfig>`:
  - existing `EngineKind::Jit` -> From reads env -> EXACTLY today's behavior (CI with X86JIT_BG_TIER unchanged, tests untouched)
  - a new caller passes an explicit EngineConfig -> bypasses env, full control
- `backend()` + tier-up application become pure methods on EngineConfig.
- BG_REGION still implies Background tier-up (regions only tier up in the bg).

## Blast radius
- 6 run_* fns (run_image / run_config / run_config_argv / _stdin / _stdin_features / _opts): `engine: EngineKind` -> `engine: impl Into<EngineConfig>`.
- 3 env reads (bg_region_enabled, HOST_BASELINE, BG_TIER) move into `from_env`.
- x86jit-run/src/main.rs, x86jit-cli/src/main.rs, x86jit-run/tests/common/mod.rs: pass EngineKind as before (Into-converts).
- OUT OF SCOPE: x86jit-tests/src/oracle.rs reads X86JIT_BG_TIER on its own backend-build path (not through run) — leave it.
- FOLLOW-UP (separate, optional): expose --superblocks / --bg-tier / --host-baseline flags on x86jit-cli building EngineConfig explicitly.

## Verify
cargo build + clippy + cargo nextest run -E 'not binary(fuzz_robustness)' (full suite green, zero behavior diff). Confirm X86JIT_BG_TIER / X86JIT_BG_REGION / X86JIT_HOST_BASELINE still take effect through the run path (via From<EngineKind>).
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 EngineConfig{kind,tier_up,superblocks,host_target} + TierUp enum added
- [ ] #2 env parsing moved into EngineConfig::from_env; no env reads in backend()/load_process
- [ ] #3 From<EngineKind> preserves today default+env behavior for existing callers
- [ ] #4 run_* fns take impl Into<EngineConfig>; main/cli/tests pass EngineKind unchanged
- [ ] #5 full suite green, X86JIT_BG_TIER/BG_REGION/HOST_BASELINE still honored via run path
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Sequencing: do TASK-182 (merge run+oci into x86jit-cli) FIRST — this EngineConfig work then lands in the merged x86jit-cli lib instead of x86jit-run. The design is unchanged; only the crate/paths move.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
