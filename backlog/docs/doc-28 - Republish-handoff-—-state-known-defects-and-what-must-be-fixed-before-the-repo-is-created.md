---
id: doc-28
title: >-
  Republish handoff — state, known defects, and what must be fixed before the
  repo is created
type: guide
created_date: '2026-08-09 14:49'
---

# Republish handoff

**Read this before touching the republish.** Rewritten 2026-08-15 at `621db28`;
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

`main` @ `621db28`, clean, **and pushed** — `origin/main` is level with it. **847 unit tests green in both debug and release**, clippy,
fmt, the aarch64 cross-check, `cargo deny`, the guest-agnostic guard, the perf gate and
the full 169-rung ladder all clean.

**Both CI lanes are green, and the AArch64 one has now actually run** — the item this
handoff carried as "the one thing never verified by execution" since it was written:

```
test (aarch64)  742 tests run: 742 passed (1 slow), 6 skipped
test (aarch64)  release profile: 240 passed  ·  32-bit lane (MODE-A): 22 passed
```

742 rather than 847 because the host-comparison suites are gated off there — on ARM there
is no host to be the oracle. The validation is carried by `interp == JIT` and
`interp == Unicorn`. `../unemulinux` @ `fbf275e`, clean.

**Every HIGH task is closed.** The board is three genuinely-blocked items, one the
maintainer parked, and one in progress.

### Done since the previous handoff (17 commits, 2026-08-13/15)

| | |
|---|---|
| `93d6320` | **compiled stores never invalidated anything.** A guest that patched another block and called it ran the stale translation under the JIT while the interpreter observed the patch. Two defects: the missing write barrier, and the compiled chain never returning to `handle_smc`. Cost, measured and accepted: ~+9 hot instructions per store, ~10% on `memcpy` |
| `19ebce7` | **fault atomicity.** 11 interp handlers committed the destination before their last faulting load; three IR ops made the lifter pre-copy into `dst` first; both tiers named the operand base instead of the faulting sub-access |
| `d846d10` | **vector MMIO looped forever.** A 16-byte access is two transfers and the answer channel carries one, so one pending value per retry could never converge |
| `fa8bfb7` `34d16ed` `8cdc50a` | **multi-vcpu soundness**: epoch-validated slot publication, `as_mut_slice` aliasing UB removed, race-free helper counters, SMC tracking across the whole address space, and the SDM cross-modifying protocol pinned across two vcpus |
| `802bc74` | `Prot` is advisory and now says so in three places, pinned by test |
| `6eb1d05` | **x87 exception flags are set**, witnessed against a real CPU — and found two defects nobody had asked about: masked overflow ignored the rounding mode, and denormalization loss was not counted as inexact |
| `00c684c` | **`#MF` is delivered**, on the instruction after the one that raised it, and the raising instruction is abandoned. The JIT variant of its test was silently running on the interpreter; once it wasn't, it exposed a real gap — `emit_x87` tested only for `RET_UNMAPPED`, so the helper's `RET_EXCEPTION` vanished |
| `03769b8` | stack **overflow** with SF and C1, host-witnessed. Underflow deliberately left: its detection point is the register READ, not the pop, and doing it in `pop()` is the shortcut that catches `fdivp` and misses `fadd st0, st3` |
| `f067726` `0ae1093` `7f9ffa0` | the x87 records narrowed three times as the ground under them moved |
| `55ddc64` | stack **underflow**, `ficom`/`ficomp`, and the denormal exception — **task-328 closed**. `st()` became `st_operand -> Option<F80>` so abandoning on an unmasked fault is a type obligation, not a comment |
| `2b64e37` | the three findings from the cloud review, all real: `vextract*` to MMIO looped forever, `widen_code_range` never converged for page 0, `fstp tbyte` skipped underflow |
| `621db28` | the AArch64 lane **could not compile the test crate**, and the pre-push guard that should have caught it was scoped to one crate |

## The open tasks, and how to pick one up

**Nothing on the board is programming work.** Two are decisions, two are blocked on
things that do not exist yet. Read the task body first (`backlog task <id> --plain`); each carries its evidence.

| id | what | note |
|---|---|---|
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

1. **~~Run the aarch64 CI lane~~ — DONE 2026-08-14, both lanes green.** Keep the note for
   the next person who wonders whether it needs `unemulinux`: it does not. `ci.yml`
   contains no reference to it or to the ladder; the lane is fmt, clippy, `cargo deny`,
   the workspace test run and a release pass, all self-contained. The only prerequisite
   was pushing, because Actions runs what is on the remote.
   The first attempt failed and the failure was worth having — see the traps below.

2. `TASK-236` or a conscious decision to publish without it, said in the README rather
   than left for a reader to discover. **Open question attached to it:** should this
   repository run `unemulinux`'s ladder at all? The maintainer's point is that
   `unemups4` is a second embedder in exactly the same relationship and its tests are
   never run here — the asymmetry is historical, not principled, and this repo even has
   a hook named "no downstream consumer named in engine source". The counterweight is
   that the ladder has caught what the ISA corpus cannot: a default `fpu_cw` of 0 meant
   24-bit precision for every guest that never runs `fldcw`, 797 unit tests passed and
   busybox `awk` did not. Suggested resolution: keep the ~30 s smoke hook until
   `TASK-236` replaces it, then drop it — and design 236 for BOTH consumers, since the
   missing `unemups4` trigger is a gap on that side rather than a reason to level down.
   Worth a `backlog decision`; the maintainer was asked and the session ended first.

3. Decide `bench/history` — it carries a hostname and CPU model.

4. Re-run: `cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)'`
   (and the same with `--release`), `cargo clippy --all-targets --all-features -- -D
   warnings`, `cargo fmt --check`, **`cargo check --workspace --target
   aarch64-unknown-linux-gnu --tests`** (workspace, not one crate — see the traps),
   `cargo deny --all-features check licenses bans sources`,
   `scripts/guest-agnostic-guard.sh`, `scripts/ladder.sh --full`,
   `cd oracles && ./fetch-oracles.sh verify`.

5. Only then: orphan branch, one signed commit, `gh repo create`.

`unemulinux` is publishable on the same schedule; see its own `backlog/`.

## Traps this work actually hit — do not relearn them

**A guard scoped to one crate cannot see the others, and it stays green while they
break.** The pre-push cross-target check ran `cargo check -p x86jit-cranelift --target
aarch64 --tests`, so it never looked at `x86jit-tests` — where four files imported the
`cfg(target_arch = "x86_64")` native oracle unconditionally. The AArch64 lane's FIRST
execution failed to compile the crate, meaning no test had ever run on ARM, while the
guard had been green throughout. It is `--workspace` now, and that form reproduces the CI
failure locally in seconds (verified by removing a gate and watching it fail).

**A cloud review found three real defects and all three were the same shape:** a
mechanical migration updated every site it could see and left behind the one site that
had nothing to see. `exec_v_extract_lane_wide_m` probed with a `vload`, so when the MMIO
answer channels split it consumed the read channel and looped forever. `FstpF80` reads
its register raw — deliberately, so a pseudo-denormal survives — so it had no `st()` call
for the underflow migration to catch. After a signature change, check who never used the
old signature, not just what the compiler flagged.

**Doc comments claiming completeness age badly.** `st_operand`'s comment said "every one
of the thirty-nine readers has to say what it does about it" while one reader did not.
The same false-completeness shape as the README claims corrected earlier in this arc.



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
| x86jit | `~/src/x86jit`, `main` @ `621db28`, pushed to `github.com/mikolajmikolajczyk/x86jit` |
| unemulinux | `~/src/unemulinux`, `main` @ `fbf275e` |
| oracles | submodule in both, `unemu-org/oracles`. The SDM is **fetch-only** — `./oracles/fetch-oracles.sh fetch` before deriving a new hardware fact |
| the LLVM bundle | `backlog/docs/llvm-i128-miscompile/` — `run.sh`, `UPSTREAM-REPORT.md` |
| fixture + renumber archive | `~/src/x86jit-fixture-mirror` — **only copy**, do not delete |
