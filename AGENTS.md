# AGENTS.md — x86jit

Repo-specific notes for coding agents (Claude Code, Cursor, Aider, Copilot, …). Generic software-engineering advice is out of scope.

> **CLAUDE.md** at repo root is `@AGENTS.md` plus any Claude-only overrides. Other agents read this file directly.

## What this is

`x86jit` is a guest-agnostic x86-64 → host recompiler (JIT) delivered as a pure-Rust library. You feed it a memory map plus an entry point; it executes guest x86-64 instructions on any host (x86-64 or ARM64) and hands control back through `Exit` whenever it hits something it doesn't handle (syscall, MMIO, unknown instruction). File-format parsing, OS syscall emulation, and devices live in the embedder's code, not the core — a Linux userland built on this library is [`unemulinux`](https://github.com/unemu-org/unemulinux), which is where the syscall shim, the process model, the OCI/ELF runner and the whole-program test ladder (busybox, sqlite, CPython, Go servers) live. That boundary is enforced, not just documented: `x86jit-core`'s dependency set is exactly `{iced-x86}` and `x86jit-tests/tests/boundary.rs` fails the build if it changes. The authoritative design is [`spec.md`](backlog/docs/design/spec.md); where the engine's *behaviour* comes from is [`PROVENANCE.md`](PROVENANCE.md).

This project tracks work **locally** with [Backlog.md](https://github.com/MrLesk/Backlog.md) — tasks, docs, and decisions are committed markdown under `backlog/`, no external issue tracker. It's forge-agnostic: a git remote (GitHub here) is an optional mirror.

## Where things live

| Need | Path | When to load |
|------|------|--------------|
| **Roadmap / task board (source of truth)** | Backlog.md — `backlog task list --plain` | Always. **Don't read roadmaps from markdown.** Milestones: `m0-skeleton`…`m8-simd`, `open-backlog`, `ps4-perf`, `go-caddy`, `code-review`. |
| **Authoritative design: contract, IR, backends, milestones** | [`spec.md`](backlog/docs/design/spec.md) + [`backlog/docs/design/`](backlog/docs/design/) | The source of truth for design. Every module cites a `spec.md` section. |
| **Where behaviour comes from: sources, oracles, licences** | [`PROVENANCE.md`](PROVENANCE.md) + [`oracles/`](https://github.com/unemu-org/oracles) | Before encoding a hardware fact, or when an oracle disagrees with the engine |
| Current repo shape, data flow, module map | [`backlog/docs/architecture.md`](backlog/docs/architecture.md) | Structural changes or unfamiliar layout |
| Coding conventions, naming, commit style, x86 semantics traps | [`backlog/docs/conventions.md`](backlog/docs/conventions.md) | Before writing or modifying code |
| Milestone status (works / in-flight / broken) | [`backlog/docs/status.md`](backlog/docs/status.md) | "Does X work?" or picking up work |
| Build / test / run / lint commands | [`backlog/docs/commands.md`](backlog/docs/commands.md) | Running build/test/dev loops |
| Interp-vs-hardware tracer (`scripts/lockstep.sh`) | [`backlog/docs/design/doc-24 - Lockstep-interp-vs-hardware-tracer.md`](backlog/docs/design/doc-24%20-%20Lockstep-interp-vs-hardware-tracer.md) | A deterministic wrong-result bug where interp==JIT but both differ from a real CPU |
| **Real-program ladder** (`scripts/ladder.sh`) | [`backlog/docs/commands.md`](backlog/docs/commands.md) § the real-program ladder | After a lifter or codegen change — the corpus validates what *is* lifted, this validates what real software needs |
| Toolchain (Nix devShell, direnv, pre-commit, `backlog`) | [`backlog/docs/dev-setup.md`](backlog/docs/dev-setup.md) | Fixing tooling, onboarding |
| Task statuses, branch naming, session handoff | [`backlog/docs/working-on-tasks.md`](backlog/docs/working-on-tasks.md) | Before picking up a task |
| Where to capture a decision (`backlog decision` vs task note) | [`backlog/docs/decisions.md`](backlog/docs/decisions.md) + `ls backlog/decisions/` | Making a non-trivial decision |
| Glossary / emulator + x86 terminology | [`backlog/docs/glossary.md`](backlog/docs/glossary.md) | Hitting an unfamiliar term |
| Deliberately deferred — do NOT implement unprompted | [`backlog/docs/deferred.md`](backlog/docs/deferred.md) | Before adding features that "seem missing" |
| ISA compatibility map (CI-tested artifact) | [`backlog/docs/compat/`](backlog/docs/compat/) | Touching the lifter's instruction coverage |
| Backlog skill (`backlog` CLI + task/doc/decision workflow) | [`.agents/skills/backlog/SKILL.md`](.agents/skills/backlog/SKILL.md) | Auto-loaded by the backlog skill trigger; also when driving `backlog` manually |
| Code navigation (`repoctx` — structure-aware, token-cheap) | `.claude/skills/repoctx/SKILL.md` | "Where is X?", callers/callees, blast radius — prefer over grep/find |

> **Skills location.** Vendored skills (e.g. `backlog`) live at `.agents/skills/<name>/` (agent-agnostic, **committed**); `scripts/skills-bootstrap.sh` symlinks them into `.claude/skills/` (gitignored) for Claude Code auto-trigger — re-run it after adding one. `repoctx` is **tool-managed**: it lives directly under `.claude/skills/repoctx/` (gitignored, regenerated by the `repoctx` CLI), not vendored.

## Load-on-demand rule

Don't read every `backlog/docs/` file at session start. Pick the file matching the task — they are sized to be loaded individually. The table above tells you *when* to load *what*. `spec.md` is long; jump to the cited section rather than reading it whole.

## Working on tasks

Read [`backlog/docs/working-on-tasks.md`](backlog/docs/working-on-tasks.md) for statuses (`To Do` / `In Progress` / `Done`), branch naming (Conventional Branch), and the forge-agnostic git flow. Milestone labels mirror `spec.md` §12. Set a task `In Progress` **before** writing code; `Done` only after it lands on the default branch. Run `backlog instructions overview` for the authoritative CLI workflow.

**Cross-repository ids carry a tag.** `task-40 (unemulinux)`, `doc-7 (unemulinux)`, `decision-4 (unemups4)` name *that* project's numbering; an untagged `doc-22` or `decision-5` is ours. Every project involved keeps its own counter, so the tag is what makes a reference resolvable — a bare number is only unambiguous inside one repository. Tag it when you write it: retro-fitting the tag after two counters have drifted means guessing which one a reader meant.

## Session handoff

> **Republish in progress.** Read
> [`backlog/docs/doc-28 - Republish-handoff…`](backlog/docs/doc-28%20-%20Republish-handoff-%E2%80%94-state-known-defects-and-what-must-be-fixed-before-the-repo-is-created.md)
> before touching it: it records what is done, the known defects, what blocks creating
> the public repository, and the traps that already cost a session. **No repository has
> been created yet** — do not run `gh repo create`.

Ending a session mid-task, leave the state on the task itself:

```sh
backlog task edit <id> --notes "Session pause $(date -I). Done: <X>. Next: <Y>. Blocker: <Z|none>."
```

Starting a session, read the most-recently-touched in-progress task (`backlog task list -s "In Progress" --plain`, then `backlog task <id> --plain`) before doing anything else.

## Quick dev loop

```sh
nix develop                 # toolchain (backlog, pre-commit, rust, cargo-deny) — or direnv auto-loads it
git submodule update --init # oracles: the pinned manuals the code cites (PROVENANCE.md)
cargo build                 # whole workspace
cargo nextest run --features unicorn -E 'not binary(fuzz_robustness)'   # fuzz excluded — >7min by design
cargo clippy --all-targets --all-features -- -D warnings
cargo deny --all-features check licenses bans sources                   # the licence gate
```

Full list: [`backlog/docs/commands.md`](backlog/docs/commands.md).

## Hard rules (don't violate)

- **Never commit without explicit user request.** Even mid-task, after accepting a plan, stop and ask. Plan acceptance ≠ commit acceptance.
- **Don't add features, refactor, or introduce abstractions beyond what the task requires.** Bug fix = bug fix, not surrounding cleanup.
- **Don't pre-empt later milestones.** `todo!()` stubs are milestone markers — fill them in `spec.md` §12 order. Don't half-implement a later milestone during earlier work.
- **Encode x86 semantics traps once, centrally** (upper-32-bit zeroing, effective-address lowering, RIP-relative, FS/GS base, flags). See `spec.md` §16 and [`backlog/docs/conventions.md`](backlog/docs/conventions.md).
- **All project docs live under `backlog/`** — the design spec and testing architecture are at [`backlog/docs/design/`](backlog/docs/design/) (`spec.md`, `testing.md`). Tasks and decisions live under `backlog/` too (`backlog/tasks/`, `backlog/decisions/`).
- **Provenance — derive behaviour from a cited primary source, and pin it with a witness test.** Every fact about how the *hardware* behaves — instruction semantics, flag effects, the x87 environment and tag-word images, rounding control, CPUID leaf/bit assignments, the initial stack and auxiliary vector — must come from and cite the **Intel SDM** (primary), the **AMD64 APM** (second witness where the SDM is silent or says "undefined"), the **x86-64 psABI**, or the **Linux uapi tables**, all pinned in [`oracles/`](#the-clean-oracle-stash-oracles). Cite by **volume and section**: `// SDM Vol 1 §8.1.7` is a citation, `// per the SDM` is not — the second cannot be checked and looks like it can. **An oracle is not an authority.** Unicorn, the host CPU and the 80286 corpus are things we compare against; they can falsify a claim but cannot license one, and when an oracle disagrees with the manual the manual wins and the divergence is recorded with the measured bytes. **Never take a semantic from reading another emulator's implementation** — naming a project as *precedent for a design choice* (the memory-ordering tiers cite Box64's `STRONGMEM` and FEX's RCpc use) is a different act from taking a fact from it. Comments and tests cite the source and say what it is — forward-only; they never narrate what a fact is *not* derived from.

## The clean-oracle stash (`oracles/`)

The manuals and tables the code cites live in **`oracles/`, a git SUBMODULE** — its own repository at [`unemu-org/oracles`](https://github.com/unemu-org/oracles) (MIT), shared with a second project. Citation paths are stable: `oracles/linux-x86/syscall_64.tbl`, `oracles/psabi/kernel.tex`, `oracles/intel/sdm.pdf`.

```sh
git submodule update --init                     # the VENDORED sources arrive with this — no download
./oracles/fetch-oracles.sh verify               # OFFLINE: committed copies vs recorded sha256
./oracles/fetch-oracles.sh fetch                # adds the fetch-only sources (the PDFs, the psABI)
./oracles/fetch-oracles.sh verify --upstream    # re-download at the pins and DIFF against the copies
```

**Vendored** (arrive with the submodule, no network): the Linux x86 syscall tables and process/signal uapi headers, the MOO container format, the SingleStepTests 80286 documentation and revocation list. **Fetch-only**, because they may not be redistributed: the **Intel SDM** and **AMD64 APM** (© Intel / © AMD, no redistribution licence) and the **x86-64 psABI** (its repository carries no licence file at all).

`verify --upstream` names its two failure modes separately because they mean opposite things: **LOCAL DRIFT** (exit 1) is a committed copy that no longer matches its recorded hash — *this* repository changed; **UPSTREAM MOVED** (exit 3) is the copies matching their sums while upstream at the pinned ref no longer does — *the source* changed under a pin, which is itself a finding worth re-reading the citations over.

> **A subagent's fresh worktree does NOT carry the submodule checkout.** Point it at the `oracles/` directory of the *main* worktree (`git -C <main-worktree> rev-parse --show-toplevel`, then `/oracles`) read-only, or symlink the stash in — otherwise it will silently derive from nothing. Do not hard-code an absolute path: it is wrong on every checkout but one.

**A clean source can still contain a second-hand block** — see `oracles/MANIFEST.md` § *Second-hand blocks in a clean source*. "It came from the stash" is not by itself proof that a fact is first-hand.

`oracles/MANIFEST.md` documents every source: name, version/commit, sha256, licence, and which fact it witnesses. The **in-code witness tests pin the cited values by value**, so `cargo test` runs with no stash and no network; the stash is for human verification and for deriving *new* facts (open the SDM at the cited section). [`PROVENANCE.md`](PROVENANCE.md) is the full audit — what is authoritative, what is only an oracle, what we deliberately do not model.

## Code ownership

Maintainer / sole decider: Mikołaj Mikołajczyk.
