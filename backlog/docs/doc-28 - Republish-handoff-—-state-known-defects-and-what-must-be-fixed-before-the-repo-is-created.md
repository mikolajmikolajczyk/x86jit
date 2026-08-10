---
id: doc-28
title: >-
  Republish handoff — state, known defects, and what must be fixed before the
  repo is created
type: guide
created_date: '2026-08-09 14:49'
---

# Republish handoff

**Read this before touching the republish.** Written 2026-08-09 at `80a16c2`.

## What we are doing

Republishing `x86jit` under [`unemu-org`](https://github.com/unemu-org) as a
production-presentable public project, alongside `oracles` (already public) and a
new sibling, `unemulinux`, which takes the Linux userland.

**No repository has been created yet.** We are still in audit and cleanup. Do not
run `gh repo create` until the checklist below is clear and the maintainer says so.

Three decisions are settled and are not open for re-litigation:

- The Linux userland is split out into `unemulinux`. Maintainer's call.
- History is discarded: both projects get a fresh single initial commit.
- The real-program ladder still runs — it just runs in `unemulinux` now. The split
  makes the CI job span two repositories; it does **not** remove coverage.

## State — what is done

17 commits on `main` since `ce2a286`. In order:

| | |
|---|---|
| `7a835fe`…`03b7326` | four instruction lifts driven by real traps (vextract mem-dst, fnstenv, x87 integer arithmetic, legacy shufps m128, fldenv) |
| `46630b6` | fixtures load at run time, not via `include_bytes!` — the workspace compiles with them absent |
| `20745d7` | **the split**: embedder, CLI, 24 test binaries and `programs/` moved out |
| `dba5dc3` `fe720ff` `4f27137` | provenance: `oracles` submodule, `PROVENANCE.md`, `deny.toml` gate, witness tests, 80286 corpus pinned |
| `27dedb6` `ad7bc2a` | backlog split to unemulinux + renumber; commit pointers stripped |
| `191544c`…`288a116` | documentation audit |
| `0e353b2` `80a16c2` | repairs from two adversarial reviews |

x86jit today: **463 tracked files, 4.2 MB**, 759 tests in ~6 s, clippy/fmt/aarch64
clean, zero broken links, no GPL binary, `x86jit-core` deps exactly `{iced-x86}`.

## Known defects and open problems

### 1. `unemulinux` does not build — the largest remaining piece

`~/src/unemulinux`, two commits, files copied **verbatim** from x86jit. Nothing has
been renamed or rewired. Missing:

- workspace `Cargo.toml` (crates: `unemulinux`, `unemulinux-cli`, `unemulinux-tests`)
- crate rename `x86jit_linux` → `unemulinux` across every moved file
- path dependencies on x86jit (switch to a git dep once x86jit is published)
- `unemulinux-tests/tests/jit_whole_program.rs` — hand-assembled here, imports unverified
- the fixture work (see §4) lands **there**, not here

Already done there: 75 tasks, 8 design docs, 4 decisions moved and renumbered to
1..66; backlog config; the sweep repair.

### 2. No CI gate on the real-program ladder — **TASK-236**

`.github/workflows/ci.yml` is `workflow_dispatch`-only, so nothing runs on push at
all, and nothing reaches unemulinux's ladder. A lifter regression that only shows up
in real software can land with ISA-level tests green. The x86jit revision under test
must be *propagated* into that job or the gate proves nothing.

### 3. x87 tag word is wrong after `fninit` — **TASK-237**

Measured on this host:

| | hardware | engine |
|---|---|---|
| `fninit; fnstenv` | `0xffff` | `0x5555` |
| `fninit; fld1; fnstenv` | `0x3fff` | `0x1555` |

`tag_word` derives tags from live `fpr[]` bytes and cannot express `11` (empty).
Pinned by `x87_tag_word_after_fninit_diverges_from_hardware`, which asserts the
**divergent** values on purpose — that test must fail and be updated when the real
fix lands. Not a blocker for publication; it is documented and visible.

### 4. Guest fixtures are still committed in `unemulinux`

~38 MB of third-party binaries came across untouched. **`lua.elf` and `python3.elf`
statically link GNU Readline and are GPL-3.0 combined works**; `busybox.elf` is
GPL-2.0; the glibc loader is LGPL-2.1. Publishing unemulinux with them redistributes
copyleft binaries with no corresponding source. They must be de-vendored (fetch +
SHA256 pin) and lua/python rebuilt **readline-free** before that repo goes public.
x86jit is already clean — it keeps only `pthreads.elf`, our own work.

The provenance of those binaries is archived at `~/src/x86jit-fixture-mirror`
(191 MB, with a README). The nixpkgs rev that built them is **lost**; `flake.lock`
predates every version in them. Do not delete that mirror.

### 5. Deliberately left alone — do not "fix" without asking

- **perf gate.** Maintainer's decision to leave as is. For the record: the baseline
  is bound to hostname `miknix-laptop`, so the gate disables on every other checkout;
  three of five workloads (`simd`, `memcpy`, `indirect`) have no baseline and hit a
  silent `continue`; `bench/history` (13 records) carries the hostname, the CPU model
  and measurements for the removed `sha256`/`sqlite`/`lua` workloads. A structural
  alternative was proposed and measured — the `Counters` fields are bit-identical
  across runs while wall clock drifts 0.8–5.8% on an idle machine — but it was not
  adopted.
- **29 files in `~/src/unemups4`** cite old x86jit task numbers. The old→new maps are
  at `~/src/x86jit-fixture-mirror/renumber/`. Deliberately kept outside both repos.

### 6. Never audited

Stated so nobody assumes it was:

- **the contents of 256 task files.** Only their references, SHAs and sweep damage
  were checked — never whether what they say is still true.
- `.github/workflows/ci.yml` beyond removing the OCI steps and adding the corpus.
- `x86jit-tests/vectors/` and `scripts/`.

## Before the repo is created

1. `unemulinux` builds and its suite passes (§1).
2. Fixtures de-vendored there, lua/python rebuilt readline-free (§4) — this is a
   **licence blocker for unemulinux**, not for x86jit.
3. Decide the CI gate (§2): implement, or publish knowingly without it and say so in
   the README rather than leaving a reader to discover it.
4. Decide what to do about `bench/history` exposing the hostname and CPU (§5).
5. Re-run: `cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)'`,
   `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check`,
   `cargo check --target aarch64-unknown-linux-gnu -p x86jit-cranelift --tests`,
   `cargo deny --all-features check licenses bans sources`,
   `cd oracles && ./fetch-oracles.sh verify`.
6. Only then: orphan branch, one signed commit, `gh repo create`. A dry run of this
   was done and reverted; the tree is identical to `main`, so it is a
   `git checkout --orphan` away.

## Traps this session actually hit — do not relearn them

- **git quotes non-ASCII paths.** Every task filename here has an em-dash, so
  `git show --name-only` + a whitespace split silently drops most files. It made a
  "complete" repair cover 20 files out of 267 and report success. Use
  `-c core.quotepath=false ... -z` and split on NUL.
- **`rtk` shims `git` and `grep`.** `git log` returned 50 commits once, which made a
  22-day history look like 8 days. `grep --include` fails outright. When a count
  looks surprising, re-run through `rtk proxy` or do it in Python.
- **A verification that checks one damage class reports clean over a broken tree.**
  Checking `corrupt(old) == current` misses every line that *also* had another edit.
  Check the property you care about, not the transformation you happen to remember.
- **`jit_eq_interp` alone cannot prove a lift exists** — an unlifted opcode traps
  identically in both tiers, so the parity test passes against a missing lift. Assert
  the run reached `Hlt` and compare against an independently derived expectation.
- **Most 7–16 hex digits in this backlog are data, not SHAs**: `3fb6cd8e` is a float
  bit pattern, `000f4240` a bench constant, `deadbeefdeadbeef` Go's `clobberfree`
  poison. A blanket sweep corrupts technical content that no test can see.
- **Never `re.sub(r"  +", " ")` or `re.sub(r"\(\s*\)", "")` over markdown.** Those two
  lines destroyed indentation in fenced blocks and every empty call-paren across 380
  files — `Result<(), E>` became `Result<, E>`. `fmt` does not read markdown, tests do
  not read the spec, and a link checker only follows links. An adversarial reader
  caught it; nothing else could have.

## Key locations

| | |
|---|---|
| x86jit | `~/src/x86jit`, branch `main` @ `80a16c2`, remote `mikolajmikolajczyk/x86jit` (archive) |
| unemulinux | `~/src/unemulinux`, no remote, does not build |
| oracles | `~/src/oracles` → `unemu-org/oracles`, x86 sources added at `601515d` |
| fixture + renumber archive | `~/src/x86jit-fixture-mirror` — **only copy**, do not delete |
| the plan | `~/.claude/plans/wise-twirling-possum.md` (revised sequencing) |
