---
id: doc-8
title: 'x86jit docs — coding-agent knowledge tree'
type: readme
created_date: '2026-07-06 11:25'
---

# x86jit docs — coding-agent knowledge tree

Load-on-demand reference for AI agents. Open the file for the task at hand — don't read everything up-front. The roadmap/task board is **Backlog.md** (`backlog task list --plain`), not markdown. The authoritative *design* is [`design/spec.md`](design/spec.md); these pages are the fast index into it.

## Files

| File | Purpose |
|------|---------|
| [`architecture.md`](architecture.md) | Workspace shape, data flow, crate/module map. |
| [`commands.md`](commands.md) | Build, test, run, lint commands. |
| [`conventions.md`](conventions.md) | Coding conventions: naming, imports, comments, commits, x86 semantics traps. |
| [`dev-setup.md`](dev-setup.md) | Toolchain (Nix flake devShell with `backlog`, direnv, pre-commit). |
| [`deferred.md`](deferred.md) | Things deliberately not implemented yet — don't add them unprompted. |
| [`glossary.md`](glossary.md) | Emulator / x86 terminology. |
| [`status.md`](status.md) | Milestone status (what works, what's in flight, what's broken). |
| [`working-on-tasks.md`](working-on-tasks.md) | Backlog.md statuses, milestones, branch naming, session handoff. |
| [`decisions.md`](decisions.md) | Where to capture a decision (`backlog decision` vs task note). |
| [`design/`](design/) | Authoritative design: `spec.md`, `testing.md`, and the milestone/track plans + briefs. |
| [`compat/`](compat/) | ISA compatibility map (CI-tested artifact: `coverage.json`, `isa-coverage.md`). |
| [`user/`](user/) | User-facing docs. |

Decisions (architecture-grade + lighter cross-cutting calls) live in [`../decisions/`](../decisions/).
