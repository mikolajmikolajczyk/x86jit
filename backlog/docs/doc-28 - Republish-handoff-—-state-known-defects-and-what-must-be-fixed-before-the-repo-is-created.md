---
id: doc-28
title: >-
  Republish handoff — state, known defects, and what must be fixed before the
  repo is created
type: guide
created_date: '2026-08-09 14:49'
---

# Republish handoff

**Read this before touching the republish.** Rewritten 2026-08-12 at `0ad93ef`;
earlier versions are in the history of this file.

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

`main` @ `0ad93ef`, clean. **799 unit tests green in both debug and release**, clippy,
fmt, the aarch64 cross-check, `cargo deny`, the guest-agnostic guard and the full
169-rung ladder all clean. `../unemulinux` @ `fbf275e`, clean.

The board is **5 open tasks plus one waiting on the maintainer**, down from 8. Three of
the four hardest were closed this session, and one new one was split out of them.

### Done since the previous handoff (15 commits)

| | |
|---|---|
| `f9aa430` | **wrong result, no trap**: `vpblendw ymm` read a fixed 16 bytes of its m256 |
| `473ab3b` | the native oracle stopped defaulting x87, MXCSR and ZMM16–31; the snapshot widened 16 → 32 vector registers |
| `052ae7f` | `cargo xfuzz --mem` — the AVX campaign with memory source operands |
| `5c0f758` | the coverage map rendered the `reg_only` column its own header promised |
| `1f31882` | two published claims were false (`F80::div` "off by 1 ULP" had been fixed) |
| `1b7dec2` `ca55bbc` | **the NaN rules**: 18 sites where the answer came from the host and the optimizer |
| `9cd2ace` | `vpblendw` mislifted when its destination aliased its register source |
| `0a49c5f` | the fuzz generator masked a shift count to the operand width, not to 5 bits |
| `fb92009` | F80 classified unnormals as ordinary numbers and destroyed NaN identity |
| `679a1b5` | the x87 control word reached `fist` and **nothing else** |
| `ed802e8` | the FPU had no stack-emptiness state; `fxsave` rotated the stack when TOP ≠ 0 |
| `46c005a` `9cd03fa` `0ad93ef` | task notes, and `task-328` split out of `task-324` |

## The five open tasks, and how to pick one up

Read the task body first (`backlog task <id> --plain`); each carries its evidence.

| id | what | note |
|---|---|---|
| `TASK-305` | trapped instructions must be resumable | HIGH. Four sites, one invariant. Needs native fault/retry witnesses; jit-vs-interp cannot see any of it, because both tiers share the shape |
| `TASK-323` | multi-vcpu soundness | HIGH. Five items, one of which is an **investigation**: do compiled stores invalidate translated pages? Answer it with a two-vcpu test before assuming either way |
| `TASK-328` | x87 FP exception flags and `#MF` | **New, and the natural successor to what just landed.** The flags round-trip but nothing sets them and nothing is delivered. `f80.rs` already knows which case it is at every site — the arms returning `F80::indefinite()` are exactly the invalid ones — so this is about returning that alongside the value |
| `TASK-236` | CI gate across the two repos | needs a token so x86jit can `repository_dispatch` unemulinux. unemulinux's side already answers `ladder`; nothing sends it |
| `TASK-327` | performance roadmap | LOW, explicitly gated: do not start an item without a workload that would show the gain |
| `TASK-229` | LLVM i128 miscompile | **In Progress, and only filing is left** — the maintainer said to leave it prepared. `backlog/docs/llvm-i128-miscompile/UPSTREAM-REPORT.md` is written. Run `./run.sh` first; it exits 1 if the toolchain got fixed |

## What the three closed tasks actually found

Worth reading before starting anything float-adjacent, because the same shape recurs.

**One root cause spanned `task-325` and `task-326`: the interpreter — the JIT's oracle —
did not implement the architectural NaN rules. It inherited them from the host CPU and
from LLVM.** Rust leaves the NaN payload of a float op unspecified and LLVM freely
commutes commutative float ops, while the x86 rule is operand-order-dependent. So
`a * b` returned one operand's payload in a debug build and the other's in a release
build **of the same source**. An oracle whose answer depends on `opt-level` is not one.

That took `cargo xfuzz --secs 300` from 14 jit-vs-interp hits to **zero divergences on
both axes over 10699 programs**.

**The x87 rule is not the SSE rule.** SDM Vol 1 Table 4-8 has separate rows: SSE takes the
first source operand; x87 prefers the QNaN over an SNaN and otherwise takes the larger
significand. Taking either from the other is wrong in both directions.

**Six hardware facts the SDM does not state** were measured through the native oracle
rather than assumed — `fld m80` is a move not a conversion, `fldcw` normalizes its
operand, the tag word does not round-trip verbatim, FXSAVE's abridged tag word is indexed
by physical register though the SDM writes "STj", FXSAVE's ST slots are top-relative, and
the f32↔f64 NaN payload shifts by 29 bits. Each is a table in a doc comment next to the
code that depends on it.

## Known defects that are staying

Recorded so nobody reads them as oversights. All are in the README's "Known gaps" with
task numbers, which is the point — the project says what is wrong with it.

- Faults are not always precise (`TASK-305`).
- No x87 FP exception is ever raised (`TASK-328`). The flags are storage that round-trips.
- MXCSR governs nothing — `deferred.md`. It is now *captured* by the native oracle and its
  control half compared; the sticky flags are captured and deliberately not compared.
- Translation-cache invalidation is not race-free against a concurrent link/IBTC
  publication (`TASK-323`). Single-vcpu execution is unaffected.
- 194 codes lift their register form but not their memory form — now listed by name in the
  coverage map's `reg_only` sections, and `cargo xfuzz --mem` reports them as
  `UnknownInstruction`, which is that leg's expected output rather than a regression.
- `FIP`/`FDP`/selectors/opcode are carried verbatim, never updated.

## Before the repo is created

1. `TASK-236` or a conscious decision to publish without it, said in the README rather
   than left for a reader to discover.
2. **Run the aarch64 CI lane by hand at least once.** Still the one thing never verified
   by execution. It now matters more than it did: `emit_fbin` gained an explicit NaN
   guard that exists *because* aarch64's rule differs from x86's, and an x86 host can only
   type-check it.
3. Decide `bench/history` — it carries a hostname and CPU model.
4. Re-run: `cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)'`
   (and the same with `--release`), `cargo clippy --all-targets --all-features -- -D
   warnings`, `cargo fmt --check`, `cargo check --target aarch64-unknown-linux-gnu -p
   x86jit-cranelift --tests`, `cargo deny --all-features check licenses bans sources`,
   `scripts/guest-agnostic-guard.sh`, `scripts/ladder.sh --full`,
   `cd oracles && ./fetch-oracles.sh verify`.
5. Only then: orphan branch, one signed commit, `gh repo create`.

`unemulinux` is publishable on the same schedule; see its own `backlog/`.

## Traps this work actually hit — do not relearn them

The recurring one, in many disguises: **a check that cannot fail reports clean over a
broken tree.** Every instance below was caught by deliberately breaking something and
watching, never by reading the code.

- **Asserting that an oracle CAPTURED something does not test that the comparator LOOKS at
  it.** The first zmm16–31 test passed with the comparator still narrowed to 16 *and* the
  interpreter dropping zmm20. Drive `compare()` directly, one field at a time.
- **A negative control can silently hit the wrong function.** One round anchored on a line
  that appears in both `exec_fxstate` and `load_env28`, patched the first, reported
  "0 failed", and nearly passed as evidence that the tests were weak. Anchor on unique
  context.
- **A test can pass because the host agrees with the rule by accident.** On x86, Rust's
  `*` compiles to `mulss`, whose NaN rule *is* the x86 rule — so removing the fix from
  `apply_f32` and re-running still passes. Stated on the test module rather than left
  implied.
- **Adding state with nothing that exercises it.** The FXSAVE emptiness side went in with
  no test; breaking its tag decode left every other x87 test green, which is how two
  indexing bugs survived to be found later.
- **The compat probe's first version reported zero memory-form gaps** — including with a
  memory form deliberately broken — because the encoder rejected every memory operand.
- **The guest-agnostic guard had no `-i`** despite a comment claiming it did.
- **A "masked EVEX" test built with iced's assembler is not masked.** Write EVEX bytes by
  hand.
- **`jit_eq_interp` alone cannot prove a lift exists** — an unlifted opcode traps
  identically in both tiers. It also cannot see a wrong lift both tiers share: that is
  what caught `vpblendw` twice.
- **A hardware reference can be wrong.** `fdivp` in AT&T syntax inverts the operands.

Two that are about *which tool* finds a thing:

- **The ladder catches what the suite cannot.** A zero default control word meant 24-bit
  precision for every guest that never ran `fldcw`. All 797 unit tests passed; busybox
  `awk`'s float `printf` did not. Run `scripts/ladder.sh --full` before believing a
  float change.
- **`cargo xfuzz` must run in `--release`** to reproduce optimizer-dependent behaviour,
  and against the **native** oracle. The NaN defect was invisible in debug.

Tooling, still true:

- **`git checkout <file>` to undo a negative control discards the whole file's work.**
  Cost a full re-apply of six changes in `interp/mod.rs` this session, and the same
  mistake had already happened once with `compare.rs`. Copy the file aside first.
- **`rtk` shims `git`, `grep` and `cat`.** `cat` strips Rust function bodies. Use
  `rtk proxy <cmd>` when a result surprises you.
- **git quotes non-ASCII paths.** Every task filename has an em-dash.
- **Slash lists survive a renumber with only their first element updated.**
- **`git add -A` before a commit whose message describes one change** produced a 74-file
  commit claiming to be about `AT_RANDOM`. Stage explicitly.
- **pre-commit refuses to run while `.pre-commit-config.yaml` is unstaged.**
- **Most 7–16 hex digits in this backlog are data, not SHAs.**
- **Never `re.sub` broadly over markdown or Rust.** One rewrote the body of the trait it
  was introducing into infinite recursion, which rustc reports as a *warning*.

## Key locations

| | |
|---|---|
| x86jit | `~/src/x86jit`, `main` @ `0ad93ef` |
| unemulinux | `~/src/unemulinux`, `main` @ `fbf275e` |
| oracles | submodule in both, `unemu-org/oracles`. The SDM is **fetch-only** — `./oracles/fetch-oracles.sh fetch` before deriving a new hardware fact |
| the LLVM bundle | `backlog/docs/llvm-i128-miscompile/` — `run.sh`, `UPSTREAM-REPORT.md` |
| fixture + renumber archive | `~/src/x86jit-fixture-mirror` — **only copy**, do not delete |
