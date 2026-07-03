# AGENTS.md — x86jit

Repo-specific notes for coding agents (Claude Code, Cursor, Aider, Copilot, …). Generic software-engineering advice is out of scope.

> **CLAUDE.md** at repo root is `@AGENTS.md` plus any Claude-only overrides. Other agents read this file directly.

## What this is

`x86jit` is a guest-agnostic x86-64 → host recompiler (JIT) delivered as a pure-Rust library. You feed it a memory map plus an entry point; it executes guest x86-64 instructions on any host (x86-64 or ARM64) and hands control back through `Exit` whenever it hits something it doesn't handle (syscall, MMIO, unknown instruction). File-format parsing, OS syscall emulation, and devices live in the embedder's code, not the core. The authoritative design is [`spec.md`](wiki/design/spec.md).

## Where things live

| Need | Path | When to load |
|------|------|--------------|
| **Authoritative design: contract, IR, backends, milestones** | [`spec.md`](wiki/design/spec.md) | The source of truth. Every module cites a section. |
| **Roadmap / backlog** | GitHub issues (`gh issue list`) | Always. Don't read roadmaps from status markdown. |
| **Ordered task backlog to a working library** | [`wiki/tasks/`](wiki/tasks/) | Planning what to build next; picking a milestone task (`M<n>-T<k>`) |
| Current repo shape, data flow, module map | [`wiki/agents/architecture.md`](wiki/agents/architecture.md) | Structural changes or unfamiliar layout |
| Coding conventions, naming, commit style, x86 semantics traps | [`wiki/agents/conventions.md`](wiki/agents/conventions.md) | Before writing or modifying code |
| Milestone status (works / in-flight / broken) | [`wiki/agents/status.md`](wiki/agents/status.md) | "Does X work?" or picking up work |
| Build / test / run / lint commands | [`wiki/agents/commands.md`](wiki/agents/commands.md) | Running build/test/dev loops |
| Toolchain (Nix devShell, direnv, pre-commit) | [`wiki/agents/dev-setup.md`](wiki/agents/dev-setup.md) | Fixing tooling, onboarding |
| Issue workflow, branch naming, PR flow, session handoff | [`wiki/agents/working-on-issues.md`](wiki/agents/working-on-issues.md) | Before picking up an issue |
| Where to capture decisions (ADR vs decision log vs comment) | [`wiki/adr/README.md`](wiki/adr/README.md) | Making a non-trivial decision |
| Glossary / emulator + x86 terminology | [`wiki/agents/glossary.md`](wiki/agents/glossary.md) | Hitting an unfamiliar term |
| Deliberately deferred — do NOT implement unprompted | [`wiki/agents/deferred.md`](wiki/agents/deferred.md) | Before adding features that "seem missing" |
| Architecture Decision Records | [`wiki/adr/`](wiki/adr/) | Touching subsystems an ADR covers |
| Cross-cutting decisions (smaller than an ADR) | [`wiki/decisions/`](wiki/decisions/) | Before reversing a prior call |

## Load-on-demand rule

Don't read every wiki file at session start. Pick the file matching the task — they are sized to be loaded individually. The table above tells you *when* to load *what*. `spec.md` is long; jump to the cited section rather than reading it whole.

## Working on issues

This repo uses **GitHub issues + pull requests**. Read [`wiki/agents/working-on-issues.md`](wiki/agents/working-on-issues.md) for the state labels (`state:*`), branch naming (Conventional Branch), and PR flow. Milestone labels mirror `spec.md` §12 (`milestone:m0`…`milestone:m8`).

## Session handoff

Ending a session mid-issue, drop a one-line comment on the active issue:

```sh
gh issue comment <n> -m "Session pause $(date -I). Done: <X>. Next: <Y>. Blocker: <Z|none>."
```

Starting a session, read recent comments on the most-recently-touched in-progress issue (`gh issue list --label state:in-progress`, then `gh issue view <n>`). Details: [`wiki/agents/working-on-issues.md`](wiki/agents/working-on-issues.md).

## Quick dev loop

```sh
nix develop                 # toolchain (or direnv auto-loads it)
cargo build                 # whole workspace
cargo nextest run           # tests
cargo clippy --all-targets --all-features -- -D warnings
```

Full list: [`wiki/agents/commands.md`](wiki/agents/commands.md).

## Hard rules (don't violate)

- **Never commit without explicit user request.** Even mid-task, after accepting a plan, stop and ask. Plan acceptance ≠ commit acceptance.
- **Don't add features, refactor, or introduce abstractions beyond what the task requires.** Bug fix = bug fix, not surrounding cleanup.
- **Don't pre-empt later milestones.** `todo!()` stubs are milestone markers — fill them in `spec.md` §12 order. Don't half-implement M4 (JIT) or M7 (multithreading) during M1 work.
- **Encode x86 semantics traps once, centrally** (upper-32-bit zeroing, effective-address lowering, RIP-relative, FS/GS base, flags). See `spec.md` §16 and [`wiki/agents/conventions.md`](wiki/agents/conventions.md).
- **All project docs live under `wiki/`** — the design spec and testing architecture are at [`wiki/design/`](wiki/design/) (`spec.md`, `testing.md`). If you find a `docs/` folder, fold it into `wiki/`.

## Code ownership

Maintainer / sole decider: Mikołaj Mikołajczyk.
