---
id: doc-10
title: 'Working on tasks'
type: guide
created_date: '2026-07-06 11:25'
---

# Working on tasks

How x86jit drives [Backlog.md](https://github.com/MrLesk/Backlog.md). The
[`backlog` skill](../../.agents/skills/backlog/SKILL.md) covers the CLI in general; this page is
the project-specific overlay. `backlog instructions overview` is the authoritative workflow guide.

## Statuses

Three statuses, nothing more.

| Status | Meaning |
|--------|---------|
| `To Do` | Filed, scoped, not started. Default for every new task. |
| `In Progress` | Actively being worked. Set it **before** you start writing code. |
| `Done` | Landed on the default branch (`main`). |

- **Exactly one status at a time.** Pick up: `backlog task edit <id> -s "In Progress"`. Finish: `backlog task edit <id> -s Done`.
- **Read with `--plain`.** `backlog task <id> --plain` and `backlog task list --plain` are the agent-friendly views.
- **Blocked?** Say so in the task (`backlog task edit <id> --notes "Blocked on <what>"`) and leave it `To Do`.

## Milestones

Work is grouped by milestone label (`-m`), mirroring `spec.md` §12 and the roadmap:

`m0-skeleton` … `m8-simd` (delivered, imported as Done history), `integration-native-diff`,
`open-backlog` (the real-program ladder + deferred/optional leftovers), `go-caddy` (the Go→caddy
roadmap — `backlog/docs/design/go-caddy-plan.md`; P2 threads is the active parent task with
subtasks `P2.0`…`P2.8`), and `code-review` (reviewed-but-deferred findings).

```sh
backlog task list -m go-caddy --plain
backlog task list -s "In Progress" --plain
```

## Task lifecycle

```sh
backlog task create "<title>" -d "<description>" --ac "<acceptance criterion>" -m <milestone>
backlog task edit <id> -s "In Progress" --plan "<how you'll approach it>"
# … work, commit (Conventional Commits, GPG-signed) …
backlog task edit <id> --check-ac 1 --notes "<what landed, any surprises>"
backlog task edit <id> -s Done            # only after it's on main
```

Don't mark `Done` until `main` actually contains the work — an early Done misleads the board.

## Git flow — this project commits to `main` directly

x86jit is solo; the maintainer's convention is **commit straight to `main`, no feature branches**
(the Backlog.md status is the "board", not git branches). Commits are **GPG-signed** (the
pre-commit `gpg-uid-guard` refuses a sign-key/email mismatch) and follow **Conventional Commits**
(`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`, `test:`, `ci:`, `build:`).

If a change is large or risky enough to want isolation, a Conventional Branch
(`<type>/<slug>`, e.g. `feat/thread-driver`) merged `--no-ff` into `main` is fine — but the default
is main. A git remote (GitHub) is an **optional mirror**; `git push` does not trigger CI (it's
manual `workflow_dispatch`, x86 + aarch64).

## Decision capture

- Task-bound decision → on the task: `backlog task edit <id> --notes "Decided: X over Y — <reason>."`
- Cross-cutting / architecture-grade → `backlog decision create "<summary>"`. See [`decisions.md`](decisions.md).

## Session handoff

```sh
backlog task edit <id> --notes "Session pause $(date -I). Done: <X>. Next: <Y>. Blocker: <Z|none>."
```

The next session reads `backlog task <id> --plain` and picks up without rediscovering state from the diff.
