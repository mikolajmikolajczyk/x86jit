---
id: doc-28
title: >-
  Republish handoff — state, known defects, and what must be fixed before the
  repo is created
type: guide
created_date: '2026-08-09 14:49'
---

# Republish handoff

**Read this before touching the republish.** Rewritten 2026-08-13 at `6eb1d05`;
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

`main` @ `6eb1d05`, clean. **831 unit tests green in both debug and release**, clippy,
fmt, the aarch64 cross-check, `cargo deny`, the guest-agnostic guard, the perf gate and
the full 169-rung ladder all clean. `../unemulinux` @ `fbf275e`, clean.

**Every HIGH task is closed.** The board is three genuinely-blocked items, one the
maintainer parked, and one in progress.

### Done since the previous handoff (8 commits, 2026-08-13)

| | |
|---|---|
| `93d6320` | **compiled stores never invalidated anything.** A guest that patched another block and called it ran the stale translation under the JIT while the interpreter observed the patch. Two defects: the missing write barrier, and the compiled chain never returning to `handle_smc`. Cost, measured and accepted: ~+9 hot instructions per store, ~10% on `memcpy` |
| `19ebce7` | **fault atomicity.** 11 interp handlers committed the destination before their last faulting load; three IR ops made the lifter pre-copy into `dst` first; both tiers named the operand base instead of the faulting sub-access |
| `d846d10` | **vector MMIO looped forever.** A 16-byte access is two transfers and the answer channel carries one, so one pending value per retry could never converge |
| `fa8bfb7` `34d16ed` `8cdc50a` | **multi-vcpu soundness**: epoch-validated slot publication, `as_mut_slice` aliasing UB removed, race-free helper counters, SMC tracking across the whole address space, and the SDM cross-modifying protocol pinned across two vcpus |
| `802bc74` | `Prot` is advisory and now says so in three places, pinned by test |
| `6eb1d05` | **x87 exception flags are set**, witnessed against a real CPU — and found two defects nobody had asked about: masked overflow ignored the rounding mode, and denormalization loss was not counted as inexact |

## The open tasks, and how to pick one up

Read the task body first (`backlog task <id> --plain`); each carries its evidence.

| id | what | note |
|---|---|---|
| `TASK-328` | x87 `#MF` delivery, stack fault, C0-C3, DE | **In Progress — AC#1 landed, three criteria left.** The next concrete step is AC#2 (SF and C1), and the shape is known: `push_raw` detects overflow when the destination is not empty, and a read of an empty register is underflow. The obstacle is mechanical, not conceptual — `st()` takes `&CpuState` and is called from ~30 sites, so raising from it needs a `&mut` migration. AC#3 needs no new `Exit`: `Exception { vector: 16 }` is `#MF` |
| `TASK-236` | CI gate across the two repos | Blocked externally: needs a token so x86jit can `repository_dispatch` unemulinux, and the target repositories do not exist yet |
| `TASK-331` | a write barrier that costs nothing | LOW. Host page protection, the Box64/FEX/QEMU answer. Wants a `backlog decision` first — it changes the embedder contract |
| `TASK-327` | performance roadmap | LOW, explicitly gated: do not start an item without a workload that would show the gain |
| `TASK-229` | LLVM i128 miscompile | **In Progress, only filing left** — the maintainer said to leave it prepared |

## What the closed tasks actually found

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

**The one that recurred all night: a green suite asserting something untrue.** Three
separate times a negative control failed to fail, and each time the TEST was weak, not the
code. The guest-SMC tests were interpreter-only, so a JIT gap was structurally invisible.
A "loop" test used two instructions at different addresses, so the RIP key alone separated
them and the clearing it was named after was never exercised. Reverting `vload`'s
sub-address broke nothing because no test covered the inner 8-byte half. **Run the
negative control before believing the test, not after.**

**Two engine defects were found by a witness nobody asked for.** Writing the x87 flag
tests against the real CPU turned up masked overflow ignoring the rounding mode (SDM Vol 1
Table 4-11 — it returned infinity for every mode) and denormalization loss not counting as
inexact. Neither was on the task's list; both were invisible to 831 passing tests. When a
fix changes nothing in the suite, that is a statement about the suite.

**Assert host-versus-expectation before engine-versus-host.** It cost nothing and caught
two wrong expectations of mine in one sitting (`1e300 * 1e300` does not overflow
double-extended; two 10-byte operands 8 bytes apart overlap), each of which would
otherwise have read as an engine bug.

## Known defects that are staying

Recorded so nobody reads them as oversights. All are in the README's "Known gaps" with
task numbers, which is the point — the project says what is wrong with it.

- x87 exception flags are set and validated against hardware, but **`#MF` is never
  delivered** — a guest that unmasks an exception gets ES set and a result, not a trap.
  The stack-fault flag, C0-C3 and DE are unmodelled (`TASK-328`).
- **`Prot` is advisory**: a store into an `R`/`RX` region succeeds and changes the bytes,
  on both backends (`TASK-330`, closed as a documented decision — the JIT has no region
  map by design, so enforcing it interpreter-side would manufacture a divergence).
- MXCSR governs nothing — `deferred.md`. It is now *captured* by the native oracle and its
  control half compared; the sticky flags are captured and deliberately not compared.
- Self-modifying code is observed **one block late** for the same-block case, the
  deviation `spec.md` §10 records and QEMU shares. Everything else invalidates on both
  backends, including across vcpus.
- 194 codes lift their register form but not their memory form — now listed by name in the
  coverage map's `reg_only` sections, and `cargo xfuzz --mem` reports them as
  `UnknownInstruction`, which is that leg's expected output rather than a regression.
- `FIP`/`FDP`/selectors/opcode are carried verbatim, never updated.

## Before the repo is created

1. `TASK-236` or a conscious decision to publish without it, said in the README rather
   than left for a reader to discover.
2. **Run the aarch64 CI lane by hand at least once.** Still the one thing never verified
   by execution — and note WHERE: the workflow lives on the *existing personal* repo
   (`github.com/mikolajmikolajczyk/x86jit`, matrix `aarch64` on `ubuntu-24.04-arm`,
   `workflow_dispatch`), not on an unemu-org repo that does not exist. The blocker is not
   the missing repo, it is that `main` is far ahead of `origin/main`: Actions runs what is
   on the remote, so it must be pushed first. Local-only verification is impossible —
   there is no aarch64 linker here, and the ARM lane rests on `interp == Unicorn`, a
   native sys-crate. It now matters more than it did: `emit_fbin` gained an explicit NaN
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
| x86jit | `~/src/x86jit`, `main` @ `6eb1d05` |
| unemulinux | `~/src/unemulinux`, `main` @ `fbf275e` |
| oracles | submodule in both, `unemu-org/oracles`. The SDM is **fetch-only** — `./oracles/fetch-oracles.sh fetch` before deriving a new hardware fact |
| the LLVM bundle | `backlog/docs/llvm-i128-miscompile/` — `run.sh`, `UPSTREAM-REPORT.md` |
| fixture + renumber archive | `~/src/x86jit-fixture-mirror` — **only copy**, do not delete |
