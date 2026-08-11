---
id: doc-28
title: >-
  Republish handoff — state, known defects, and what must be fixed before the
  repo is created
type: guide
created_date: '2026-08-09 14:49'
---

# Republish handoff

**Read this before touching the republish.** Rewritten 2026-08-11 at `b9a9543`;
the 2026-08-09 version is in the history of this file.

## What we are doing

Republishing `x86jit` under [`unemu-org`](https://github.com/unemu-org) as a
production-presentable public project, alongside `oracles` (already public) and a new
sibling, `unemulinux`, which takes the Linux userland.

**No repository has been created yet.** Do not run `gh repo create` until the
maintainer says so.

Three decisions are settled and not open for re-litigation:

- The Linux userland is split into `unemulinux`. Maintainer's call.
- History is discarded: both projects get a fresh single initial commit.
- The real-program ladder still runs — in `unemulinux` now. The split makes the CI job
  span two repositories; it does **not** remove coverage.

## Where things stand

`main` @ `b9a9543`, clean. **767 unit tests + 169 ladder rungs green**, clippy, fmt,
the aarch64 cross-check, `cargo deny`, and the guest-agnostic guard all clean.
`../unemulinux` @ `fbf275e`, clean, builds and passes its own suite.

Everything a reader sees is now true as far as anyone has checked: the publication
blockers are closed, the board is **8 open tasks**, and each is either measured,
reproduced, or explicitly an investigation.

### Done since the previous handoff (22 commits)

| | |
|---|---|
| `3251466` | `scripts/ladder.sh` — run unemulinux's real-program ladder from here |
| `c8fdd8d` `72fb252` | id gaps closed: decisions 1–10, docs 1–28, cross-repo refs tagged |
| `e4ed365` | workspace dependency comments described crates that had moved |
| `6f7191b` `6ef70ae` `76b61c5` `c68db0a` `c51322d` | −1088 lines: five duplications folded |
| `43384e6` | **jit ≠ interp**: a VEX.128 write left `zmm_hi` stale under `--cpu v4` |
| `56a76da` | the comparator treated missing/short memory as "no divergence" |
| `82c0d78` | ELF: validate before mapping — a hostile image could panic or poison the `Vm` |
| `194e19c` | two published claims narrowed to what is actually checked |
| `8da16a5` `c683bc8` `2dc15d7` | seven whole-repo adversarial reviews filed, then 43 open tasks → 10 |
| `f4f9e31` | guest-agnostic guard + the 63 downstream references it found |
| `40688a4` | `AT_RANDOM` and process identity are the embedder's to choose |
| `fb99ebe` | `lock adc/sbb` and masked EVEX scalar moves reject instead of mislifting |
| `4acac26` | **F80 rounding**: `div` and `sqrt` were wrong on 709 of 1799 measured cases |
| `bc5c451` | the LLVM i128 miscompile is a regression (20.1.8 → 21.1.8), report ready |
| `b9a9543` | the compat probe finally tests memory forms — 194 codes were over-reported |

## The eight open tasks, and how to pick one up

Read the task body first (`backlog task <id> --plain`); each carries its evidence.

**`TASK-325` is mid-flight and is the natural next step.** One of its three parts is
done and committed; the other two are untouched:

- ✅ the compat probe now builds memory forms (`b9a9543`)
- ⬜ **the AVX fuzz campaign generates no memory operands** (`fuzz.rs` ~1358). The VEX
  pool's common shape emits YMM registers, so the campaign cannot falsify memory-source
  decoding, effective-address computation, load width, alignment or fault behaviour for
  anything it counts as covered. This is the same blind spot the probe had, in the
  other tool.
- ⬜ **the native oracle captures no MXCSR, no x87 state and no ZMM16–31**
  (`native.rs` ~541). Until it does, the README's three-way claim stays narrowed —
  `194e19c` added that caveat and this task's acceptance criterion is to remove it.

The other seven, roughly by how self-contained they are:

| id | what | note |
|---|---|---|
| `TASK-229` | LLVM i128 miscompile | **only filing is left.** `backlog/docs/llvm-i128-miscompile/UPSTREAM-REPORT.md` is written; posting to an external tracker is the maintainer's call. Run `./run.sh` first — it exits 1 if the toolchain got fixed. |
| `TASK-236` | CI gate across the two repos | needs a token/secret so x86jit can `repository_dispatch` unemulinux. unemulinux's side already answers `ladder`; nothing sends it. |
| `TASK-326` | FMA and `vdpps` diverge from hardware | start with `vdpps` jit-vs-interp (seed 26816) — it violates the hard invariant and is the thread most likely to explain the rest |
| `TASK-305` | trapped instructions must be resumable | four sites, one invariant. Needs native fault/retry witnesses; jit-vs-interp cannot see any of it, because both tiers share the shape |
| `TASK-324` | x87/F80 fidelity | control word does not reach arithmetic, `fldenv`/`fnstenv` does not round-trip, tag word cannot say "empty", unnormals and NaN identity |
| `TASK-323` | multi-vcpu soundness | five items. `TASK-323`'s last one is an **investigation**: do compiled stores invalidate translated pages? Answer it with a two-vcpu test before assuming either way |
| `TASK-327` | performance roadmap | explicitly gated: do not start an item without a workload that would show the gain. `task-217` recorded four micro-optimisations that measured well locally and moved the real workload by zero |

## Known defects that are staying

Recorded so nobody reads them as oversights. All are in the README's "Known gaps" with
task numbers, which is the point — the project says what is wrong with it.

- Faults are not always precise (`TASK-305`).
- 80-bit x87 arithmetic ignores the control word's rounding and precision (`TASK-324`).
  Note `div`/`sqrt` rounding itself is now correct and pinned against hardware.
- Translation-cache invalidation is not race-free against a concurrent link/IBTC
  publication (`TASK-323`). Single-vcpu execution is unaffected.
- 194 codes lift their register form but not their memory form. Now visible in the
  coverage map's `reg_only` column instead of hidden inside `lifted`.
- MXCSR is storage, not something that governs vector arithmetic — `deferred.md`.

## Before the repo is created

1. `TASK-236` or a conscious decision to publish without it, said in the README rather
   than left for a reader to discover.
2. **Run the aarch64 CI lane by hand at least once.** It is the one thing never
   verified by execution: `barrier_tests` is `cfg(target_arch = "aarch64")`, so an x86
   host can only type-check it. `6f7191b` changed that module.
3. Decide `bench/history` — it carries a hostname and CPU model.
4. Re-run: `cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)'`,
   `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check`,
   `cargo check --target aarch64-unknown-linux-gnu -p x86jit-cranelift --tests`,
   `cargo deny --all-features check licenses bans sources`,
   `scripts/guest-agnostic-guard.sh`, `scripts/ladder.sh --full`,
   `cd oracles && ./fetch-oracles.sh verify`.
5. Only then: orphan branch, one signed commit, `gh repo create`.

`unemulinux` has its own blocker: ~38 MB of guest fixtures are de-vendored there
already, but see its `backlog/` — `TASK-236` is mirrored and the fixture licence work
is done. It is publishable on the same schedule.

## Traps this work actually hit — do not relearn them

The recurring one, in five different disguises: **a check that cannot fail reports
clean over a broken tree.** Every instance below was caught by deliberately breaking
something and watching, never by reading the code.

- **The compat probe's first version reported zero memory-form gaps — including with a
  memory form deliberately broken.** The encoder rejects a memory operand whose
  displacement is set while `displ_size` is 0, so every memory form came back
  `Unencodable`, which the probe read as "this `Code` has no memory form".
- **The guest-agnostic guard had no `-i`** despite a comment claiming it did, so a
  planted `Celeste` passed.
- **A "masked EVEX" test built with iced's assembler is not masked.**
  `a.vmovss(xmm0.k1(), ..)` silently encodes VEX and drops the mask; the test exercised
  the unmasked path and passed against the unfixed lifter. Write EVEX bytes by hand.
- **`jit_eq_interp` alone cannot prove a lift exists** — an unlifted opcode traps
  identically in both tiers.
- **A hardware reference can be wrong.** The first F80 comparison used `fdivp` in AT&T
  syntax, which inverts the operands, so it compared against `y/x`. It showed
  `1/10 = 10` and 1560 "failures".

Also worth keeping:

- **git quotes non-ASCII paths.** Every task filename has an em-dash; `--name-only`
  plus a whitespace split silently drops most of them. Use `-c core.quotepath=false`
  and split on NUL.
- **`rtk` shims `git`, `grep` and `cat`.** `cat` strips Rust function bodies, which
  makes an intact file look gutted. `git log` truncated to 50 commits once and made a
  22-day history look like 8. Use `rtk proxy <cmd>` or Python when a result surprises
  you.
- **Slash lists survive a renumber with only their first element updated.**
  `decision-11/12/13` became `decision-8/12/13`. This trap is recorded here and was
  still walked into a second time.
- **`git add -A` before a commit whose message describes one change** produced a
  74-file commit claiming to be about `AT_RANDOM`. Stage explicitly.
- **pre-commit refuses to run while `.pre-commit-config.yaml` is unstaged**, which
  forces commit order opposite to the logical one.
- **Most 7–16 hex digits in this backlog are data, not SHAs**: `3fb6cd8e` is a float
  bit pattern, `deadbeefdeadbeef` Go's poison value.
- **Never `re.sub(r"  +", " ")` or `re.sub(r"\(\s*\)", "")` over markdown.** Those two
  destroyed indentation in fenced blocks and every empty call-paren across 380 files.
  A blanket substitution is also broad enough to match its own definition — one
  rewrote the body of the trait it was introducing into infinite recursion, which rustc
  reports as a *warning*.

## Key locations

| | |
|---|---|
| x86jit | `~/src/x86jit`, `main` @ `b9a9543` |
| unemulinux | `~/src/unemulinux`, `main` @ `fbf275e`, builds, 169 tests |
| oracles | submodule in both, `unemu-org/oracles` |
| the LLVM bundle | `backlog/docs/llvm-i128-miscompile/` — `run.sh`, `UPSTREAM-REPORT.md` |
| fixture + renumber archive | `~/src/x86jit-fixture-mirror` — **only copy**, do not delete. `loose/` holds an uncommitted fuzz-divergence vector rescued from a deleted worktree |
