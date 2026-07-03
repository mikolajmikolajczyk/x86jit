# Working on issues

How this project tracks work with GitHub issues + pull requests. Solo project — keep it light.

## Issue state (labels)

Two state labels by default. No triage column (the default open list does that job), no review column (solo — no review gate).

| Label | Meaning |
|-------|---------|
| `state:in-progress` | Actively being worked. Apply **before** you start writing code. |
| `state:blocked` | Waiting on something external (decision, upstream, hardware). Pair with a `blocked:*` label that names the blocker. |
| (no state label) | Filed, scoped, not started — sits in the default open list. Default for every new issue. |

Conventions:

- **Exactly one `state:*` label at a time.** Picking up: add `state:in-progress`. Blocking: swap it for `state:blocked`. Finishing: close the issue (a closed issue ignores state labels).
- **Don't introduce `state:review`** unless a second contributor joins.
- **`state:blocked` requires a paired `blocked:*` label** (issue ref or free-text). A naked `state:blocked` is invisible — nobody knows what's blocking.

Milestone labels mirror spec.md §12: `milestone:m0`…`milestone:m8`. Priority optional: `priority:high|med|low`.

```sh
gh issue list
gh issue list --label state:in-progress
gh issue view <n>
gh issue create --title "<x>" --label "milestone:m1" --label "priority:high"
gh issue edit <n> --add-label state:in-progress
gh issue close <n>
```

## Branch naming — Conventional Branch

[conventionalbranch.org](https://conventionalbranch.org/) for any branch that isn't `main`:

```
<type>/<short-slug>
```

Types: `feat`, `bugfix`, `hotfix`, `chore`, `docs`, `test`, `release`. Optional issue number prefix if it helps you find the branch later.

```
feat/interpreter-add-sub
feat/12-effective-address-lowering     # with issue hint
chore/clippy-deny-warnings
docs/adr-0001-backend-interface
```

Conventional Branch is **not** Conventional Commits — commit messages still follow Conventional Commits separately (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`, `release:`).

## PR description template

Put this in the PR body:

```markdown
## Why

<one paragraph: motivation, link to issue #n>

## What

<bulleted summary of the changes>

## Acceptance

- [ ] criterion 1 from the issue
- [ ] criterion 2

## Notes

<anything reviewers / future-you should know>
```

## Issue → PR → merged flow

```sh
# 1. Start
gh issue edit <n> --add-label state:in-progress

# 2. Branch
git checkout -b feat/<n>-<slug>

# 3. Work + commit (Conventional Commits, GPG-signed) — only on explicit request
git commit -m "feat: <subject> (#<n>)"

# 4. Push + open PR
git push -u origin HEAD
gh pr create --fill

# 5. After merge into main
gh issue close <n>
```

Don't close an issue until `main` actually contains the merge.

## Decision capture

- Tied to one issue → **comment on the issue**: `gh issue comment <n> -m "Decided: <choice> over <alt> — <reason>. Revisit if <condition>."`
- Cross-cutting, not one issue → `wiki/decisions/`.
- Library-shape / contract → an ADR in `wiki/adr/`. See [`../adr/README.md`](../adr/README.md) for the three-way split.

## Session handoff

Ending a session mid-issue, leave a comment on the active issue:

```sh
gh issue comment <n> -m "Session pause $(date -I). Done: <X>. Next: <Y>. Blocker: <Z|none>."
```

The next session (you or an agent) reads recent comments via `gh issue view <n>` and picks up without rediscovering state from the diff.

For Claude Code specifically, the same handoff also persists in auto-memory (`~/.claude/projects/<encoded-cwd>/memory/`). Use whichever fits — issue comments are the canonical, agent-agnostic surface.
