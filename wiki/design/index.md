# Design

Authoritative design documents for `x86jit`, in English.

| Document | What |
|----------|------|
| [`spec.md`](spec.md) | The engine spec (v0.3): public contract, guest state, IR, lift, backends, cache, dispatcher, SMC, multithreading, milestones, and the consolidated pitfalls list. Source of truth. |
| [`testing.md`](testing.md) | Testing architecture (v0.1): the oracle problem, test vectors, differential fuzzing, optimization testing axes, and per-milestone test integration. |

Section references throughout the wiki and source comments (`§7.1`, `§8.2.1`, …) point at `spec.md`; `T§`-prefixed references point at `testing.md`. The [`../tasks/`](../tasks/) backlog is derived from both.
