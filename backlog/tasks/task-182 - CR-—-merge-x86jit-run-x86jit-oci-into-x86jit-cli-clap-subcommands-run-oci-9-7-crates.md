---
id: TASK-182
title: >-
  CR — merge x86jit-run + x86jit-oci into x86jit-cli (clap subcommands: run +
  oci), 9->7 crates
status: Done
assignee: []
created_date: '2026-07-09 11:13'
updated_date: '2026-07-09 11:35'
labels:
  - CR
dependencies: []
ordinal: 206000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Collapse three crates into one: merge `x86jit-run` + `x86jit-oci` into `x86jit-cli` (lib + a single `x86jit-cli` binary with clap subcommands). Reduces the workspace from 9 crates to 7.

## Why
`run`'s only consumer is `cli`; `oci`'s only consumers are `run` + `cli`; `cli` is a leaf binary. Nothing else (tests/bench) depends on run or oci — the dep graph makes this a clean, contained cut. Two hand-rolled arg parsers (cli/main.rs + run/main.rs) collapse into one clap CLI.

## Target shape
- **One crate `x86jit-cli`** = lib + one bin.
  - lib: today's `run_*` orchestration API (run_image / run_config_argv_* / RunResult / EngineKind / RunError) **+** the OCI loader as `mod oci` (load_image / ImageConfig / OciError).
  - bin (clap derive), subcommands:
    - `run <BINARY> [GUEST_ARGS]...` — run a host x86-64 ELF (today's x86jit-cli): `--backend interp|jit`, `--cpu`, `--rootfs`, `--lib`, `--env`, `--no-inherit-env`, `--quiet`.
    - `oci <IMAGE.tar>` — run an OCI/Docker image (today's x86jit-run bin): `--backend interp|jit|both`.
  - Recommended: make `run` the default subcommand so bare `x86jit-cli /usr/bin/echo hello` still works (clap `arg_required_else_help` + a default/fallback subcommand, or `#[command(subcommand)]` with a top-level fallback).
- **Delete crates `x86jit-run` and `x86jit-oci`** from workspace `members`.
- The `x86jit-run` binary name goes away — OCI is now `x86jit-cli oci ...` (this is the whole point).

## Boundary note (oci)
`x86jit-oci` today is compile-enforced to NOT depend on `x86jit-core` (its strongest boundary claim). As a `mod oci` inside `x86jit-cli` that enforcement is lost. Preserve the intent: keep the module free of any `x86jit_core` import + add a small guard (a doc note, and optionally a test/CI grep asserting `mod oci` imports nothing from core). Acceptable trade for a solo maintainer — the property was a statement of architecture, not load-bearing.

## Mechanics / blast radius
- `x86jit-cli/Cargo.toml` gains: x86jit-core, x86jit-cranelift, x86jit-elf, x86jit-linux (from run) + serde, serde_json, tar, flate2 (from oci) + clap. Drop x86jit-run / x86jit-oci deps.
- Add `clap` (derive feature) to workspace `[workspace.dependencies]` (not present yet).
- Move source: run/src/lib.rs -> cli/src/lib.rs (merge with cli's current lib logic); oci/src/lib.rs -> cli/src/oci.rs; rewrite cli/src/main.rs as the clap dispatcher (fold run/src/main.rs's OCI path in as the `oci` subcommand).
- Move tests: run/tests/{alpine,busybox,glibc,hello_world,httpd,registry_pull,shell,ubuntu}.rs + common/mod.rs and oci/tests/hello_world.rs into x86jit-cli/tests/. Rename `x86jit_run::` / `x86jit_oci::` -> `x86jit_cli::`. Keep each test's existing gating (network/registry-heavy ones stay gated as-is).
- Update the root README workspace table (9 -> 7) and the per-crate READMEs: delete x86jit-run/README.md + x86jit-oci/README.md, fold their content into x86jit-cli/README.md (document both subcommands).
- Update AGENTS.md / architecture.md crate lists if they enumerate run/oci.

## Relationship to TASK-181 (EngineConfig)
TASK-181 (EngineConfig: tier-up/superblocks/host-target off env) lands in the SAME run_* code. Do this merge FIRST, then 181 applies cleanly inside the merged `x86jit-cli` lib (or fold 181 in as a follow-up commit). Cross-reference both.

## Verify
cargo build + clippy --all-targets --all-features -D warnings + cargo nextest run -E 'not binary(fuzz_robustness)' (full suite green, incl. the moved integration tests). Confirm both CLI paths still work: `x86jit-cli run /usr/bin/echo hi` and `x86jit-cli oci <image.tar> --backend both`. No behavior change vs today's two binaries.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 x86jit-run + x86jit-oci deleted; their code lives in x86jit-cli (lib + mod oci)
- [ ] #2 single x86jit-cli binary with clap subcommands run (host ELF) and oci (image)
- [ ] #3 mod oci imports nothing from x86jit-core; guarded by note/test
- [ ] #4 moved integration tests pass (renamed to x86jit_cli::), full suite green
- [ ] #5 READMEs + workspace member list + architecture.md updated to 7 crates
- [ ] #6 both paths verified: x86jit-cli run <bin> and x86jit-cli oci <tar> --backend both
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
