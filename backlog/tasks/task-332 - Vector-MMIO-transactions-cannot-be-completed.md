---
id: TASK-332
title: Vector MMIO transactions cannot be completed
status: To Do
assignee: []
created_date: '2026-08-13 16:55'
labels: []
dependencies: []
priority: medium
ordinal: 368000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
MEASURED: a >8-byte vector access to a `RegionKind::Trap` region traps forever. Four rounds of `run` -> `complete_mmio_read` produce four identical exits:

    round 0: MmioRead { addr: 16384, size: 16 }
    round 1: MmioRead { addr: 16384, size: 16 }
    round 2: MmioRead { addr: 16384, size: 16 }
    round 3: MmioRead { addr: 16384, size: 16 }

`exec_v_load`/`exec_v_store` (interp/vector.rs) re-call `vload`/`vstore` unconditionally and never consume the value or acknowledgement the embedder installed. The scalar path does consume it (`interp/integer.rs:491` for reads, `:519` for writes) — the vector path simply never grew the same arm.

Pinned by `x86jit-tests/tests/fault_atomicity.rs::vector_mmio_read_cannot_be_completed_yet`, which asserts the CURRENT broken behaviour on purpose so that fixing it fails the test and forces the records to be updated.

WHY IT IS NOT A ONE-LINE FIX, and why it was split out of task-305 rather than closed with it. A 16-byte access is TWO 8-byte transfers, while `Exit::MmioRead`'s answer channel (`complete_mmio_read(u64)`) carries ONE value. Copying the scalar arm cannot converge: the retry re-executes the whole instruction, so the first half consumes the pending value, the second half traps, the embedder answers, the retry starts over — and now the FIRST half traps with nothing pending. Forever.

Converging needs one of:

- **Per-instruction progress state.** The vcpu remembers which sub-transfer of the faulting instruction is already satisfied. Most faithful, most machinery, and it interacts with task-305's fault-atomicity invariant (a partially-satisfied instruction must still commit nothing until every transfer has succeeded).
- **A defined refusal.** A new `Exit` variant, or a documented error, for a vector access to a Trap region. Small, honest, and admits the feature does not exist — but it is embedder-visible API surface, which is the maintainer's call.

Related, same root: a 16-byte vector STORE to a Trap region reports `Exit::MmioWrite { size: 16, value }` where `value` is only `v as u64` — half the transaction, announced as whole. Whichever direction is chosen has to fix that too.

Worth a `backlog decision`: both options change what the embedder can rely on.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A vector load/store to a Trap region either completes after the embedder answers, or is refused with a defined error — it never loops
- [ ] #2 A 16-byte MMIO write no longer announces size 16 while carrying 8 bytes of value
- [ ] #3 vector_mmio_read_cannot_be_completed_yet is replaced by a test of the chosen behaviour, on both backends
- [ ] #4 A decision record states which of the two directions was taken and what it costs the embedder
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
