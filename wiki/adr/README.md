# When to write an ADR

ADRs are append-only records of decisions that are **expensive to reverse**, **constrain future choices**, or **need explaining a year from now**. They are not a journal of every change. The bar for adding one is deliberately high so the index stays scannable.

## The three-way split

This project captures decisions in three places. Pick the right one:

| Surface | Use when | Lifetime |
|---------|----------|----------|
| **ADR** (this folder) | Decision constrains library shape, public contracts, layering, error/test/runtime semantics. Hard to reverse. Affects every future contributor. | Project-lifetime, append-only |
| **Decision log** ([`../decisions/`](../decisions/)) | Cross-cutting tool / library / process choice not tied to a single issue. Reversible in days. Examples: which crate fills a small role, whether generated artifacts ship in-tree or get rebuilt in CI, AI-agent permissions. | Until superseded; lightweight |
| **Issue comment / commit message** | Decision tied to one issue or one commit. Examples: "for `c5aaf5a` we chose encoding X over Y because of constraint Z." | Bound to that issue / commit |

If a decision spans more than the immediate work but isn't an architectural promise, it belongs in `../decisions/` — not as a fourth ADR. ADR overhead (full template, ceremony, numbered slot, append-only discipline) is wasted on a crate swap.

## Write an ADR when the decision

- **Constrains the shape of the library or public contracts** — once shipped, downstream code depends on it. (E.g. the `Exit`/`StepResult` boundary, the compiled-block ABI in §8.2.1.)
- **Is hard to reverse** — undoing it requires a migration, not a refactor.
- **Affects cross-cutting concerns** — touches multiple crates / layers / milestones.
- **Was contested or non-obvious** — there were real alternatives and someone, future-you included, will want the rationale.
- **Has stakeholder implications** — distribution, licensing, MSRV.

## Skip the ADR when the decision

- Is a **tool choice** that can be swapped in a day (formatter, linter, devShell tech).
- Is **DX convenience** with no behavioral effect (editor config, direnv, shell aliases).
- Is a **crate swap** in a single layer with no contract change.
- Belongs in a **PR description, commit message, or code comment** because it only affects that change.
- Is **a status update or roadmap item** — those live in GitHub issues, not ADRs.

## Concrete examples

### ADR-worthy

| Topic | Why |
|-------|-----|
| Workspace crate split + dependency direction | Constrains every future import; load-bearing (§2) |
| Backend interface: `materialize` + uniform `execute` over `CachedBlock` | Locks the interpreter/JIT contract (§8) |
| Compiled-block ABI (`CpuState` via pointer + `#[repr(C)]` offsets, `u64` result encoding) | External contract between codegen and dispatcher (§8.2.1) |
| RIP-on-trap-out convention (after `syscall`, on the faulting insn for MMIO) | Cross-cutting; interpreter and JIT must agree (§8) |
| Flags strategy (materialized Variant A now, lazy Variant B later) | Shapes IR; hard to retrofit (§3.2) |
| Budget unit (blocks vs instructions) | Affects the scheduling contract (§5.1) |
| License choice | Stakeholder + distribution implications (§15) |

### NOT ADR-worthy

| Topic | Where it lives instead |
|-------|------------------------|
| Nix flake devShell (or rustup choice) | [`../agents/dev-setup.md`](../agents/dev-setup.md) |
| Direnv `.envrc` | [`../agents/dev-setup.md`](../agents/dev-setup.md) |
| Pre-commit framework + hook list | [`../agents/dev-setup.md`](../agents/dev-setup.md) and `.pre-commit-config.yaml` |
| Formatter / linter choice | Config file + dev-setup page |
| Editor recommendations | dev-setup page |
| Small-role crate swap (no contract change) | PR description + code comment |
| Bumping a pinned dependency | Commit message |

### Edge cases — write an ADR if the answer is "yes"

- **Tool choice with lock-in:** "Build *requires* Nix" → ADR. "Nix is primary, host rustup works as fallback" → no ADR.
- **Crate swap that changes a public interface:** if downstream code notices the change → ADR. If purely internal → no ADR.
- **Process / workflow decision** → ADR if it's a durable contract with collaborators; skip if it's a personal preference.

## Format

Use existing ADRs in this directory as the template. Minimum sections:

- **Status** — Proposed / Accepted / Superseded by ADR-NNNN
- **Date** — ISO date of acceptance
- **Deciders** — names
- **Tags** — short labels for searchability
- **Context** — what's the situation
- **Decision drivers** — what matters in the call
- **Considered options** — alternatives, briefly
- **Decision outcome** — what we picked + why
- **Positive / Negative consequences**
- **Links** — issues, prior art, related ADRs, spec.md section

Keep ADRs short. The point is a durable trace, not a research paper. If it grows past ~250 lines, split or scope down.

## Append-only discipline

Once Accepted, do **not** edit substance. To change direction:

1. Write a new ADR that supersedes the old one.
2. Update the old ADR's Status line to `Superseded by ADR-NNNN`.
3. Add a back-link to the new ADR.

Editing typos and formatting is fine. Editing decisions is not.
