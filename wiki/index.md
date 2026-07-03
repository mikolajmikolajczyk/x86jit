# x86jit wiki

Project knowledge tree. Two audiences:

- [`agents/`](agents/) — for coding agents (Claude Code, Cursor, Aider, ...). Sized for load-on-demand. Entry: [`../AGENTS.md`](../AGENTS.md) pointer table (if present).
- [`user/`](user/) — for humans (end users, contributors). Library usage, tutorials, examples.

Cross-cutting:

- [`tasks/`](tasks/) — ordered work backlog to reach a working library, one file per milestone (from spec.md + testing.md).
- [`adr/`](adr/) — Architecture Decision Records. Lasting, library-shaping decisions.
- [`decisions/`](decisions/) — Cross-cutting decision log. Smaller than ADRs, bigger than commit messages.

The authoritative design spec lives at [`design/spec.md`](design/spec.md). The wiki is derived, load-on-demand knowledge; spec.md is the source of truth for the contract and milestones.
