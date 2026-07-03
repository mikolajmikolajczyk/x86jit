# Decision log

Mid-weight decisions that don't qualify as ADRs but are too durable for a single commit message or PR description. Append-only, lightweight, grep-friendly.

## When to write here vs ADR vs commit / issue comment

| Where | When |
|-------|------|
| **ADR** (`../adr/`) | Constrains library shape or public contracts. Hard to reverse. Affects future contributors. Examples: backend interface, compiled-block ABI, RIP-on-trap convention, flags strategy. |
| **Decision log** (this folder) | Cross-cutting tool / crate / process choice not tied to one issue. Reversible in days, not months. Examples: "we use crate A over B for role X", "generated artifacts checked in not built per-CI", "AI agents in this repo write commit messages but never push". |
| **Issue comment** | Decision tied to a specific GitHub issue. |
| **Commit message body** | Decision tied to a specific commit. Examples: "switched from `HashMap` to `Vec` for the temp map — dense small keys, no hashing cost in the hot path". |

See [`../adr/README.md`](../adr/README.md) for the ADR bar in detail.

## Format

One markdown file per decision. Name: `YYYY-MM-DD-short-slug.md`. Keep each entry under ~50 lines — long entries probably want to be ADRs.

Template:

```markdown
# <One-line decision summary>

**Date:** YYYY-MM-DD
**Decider:** <name>
**Tags:** crate-choice | process | tooling | ...

## Context

What prompted the decision. One paragraph.

## Decision

What we picked. One paragraph.

## Alternatives considered

- **Option A** — short reason it lost
- **Option B** — short reason it lost

## Trigger to revisit

What would make us re-open this decision.
```

## Index

(none yet — first entry lands when a non-ADR cross-cutting decision gets made)
