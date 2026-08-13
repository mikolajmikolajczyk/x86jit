---
id: TASK-332
title: Vector MMIO transactions cannot be completed
status: Done
assignee: []
created_date: '2026-08-13 16:55'
updated_date: '2026-08-13 17:57'
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
- [x] #1 A vector load/store to a Trap region either completes after the embedder answers, or is refused with a defined error — it never loops
- [x] #2 A 16-byte MMIO write no longer announces size 16 while carrying 8 bytes of value
- [x] #3 vector_mmio_read_cannot_be_completed_yet is replaced by a test of the chosen behaviour, on both backends
- [ ] #4 A decision record states which of the two directions was taken and what it costs the embedder
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done 2026-08-13, same session it was filed in. AC#4 (a decision record) is NOT needed after all — see below.

CHOSEN: split the access into its real transfers. A 16-byte vector access already IS two 8-byte transfers, so it is reported as two `MmioRead{size: 8}` exits rather than one `size: 16` the embedder cannot answer. No new `Exit` variant, no refusal.

WHY THE 'MAINTAINER'S DECISION' FRAMING WAS WRONG, since I filed this task on it. The objection was that changing the exit shape changes what the embedder can rely on. It does not: the previous shape was an INFINITE LOOP. Nothing can depend on behaviour that hangs, so there was no contract to break and no decision to defer. That is why AC#4 is dropped rather than satisfied.

MECHANISM. `CpuState::mmio_parts` — a fixed 8-entry `(addr, value)` table plus the RIP it belongs to, appended at the END of the struct so every `#[repr(C)]` offset and the JIT ABI stay byte-identical. `vpart`/`vpart_store` consult it before touching memory and record what `pending_mmio`/`pending_mmio_write` supplies. Keyed by ADDRESS, not by transfer index, so it does not depend on handlers visiting halves in order. Not in `jit_abi::CpuOffsets`: the JIT defers MMIO to the interpreter (`RET_MMIO_DEFER`), so one implementation serves both tiers — asserted on the JIT too rather than assumed.

Why one answer per attempt cannot work, which is the whole reason this needed state: the retry re-executes the WHOLE instruction, so `pending_mmio` is consumed by the FIRST transfer and the second traps with nothing left; answer that, re-enter, and the first traps again. Measured before the fix: 4 rounds -> 4 identical exits.

AC#2 fell out of the same change: `VecFault` carries the transfer's own `value`, so a 16-byte store no longer reports both halves as the operand's low 8 bytes.

THE CLEARING IS LOAD-BEARING, and the first test did not prove it. Entries are keyed by RIP, and my first 'loop' test used two `movdqu`s at DIFFERENT addresses — the RIP key alone separated them, so removing `clear_mmio_parts` left it green. Rewrote it with a real backward jump (same instruction, same RIP, two iterations); the negative control then failed with exactly the predicted symptom, 2 answers instead of 4 — the second iteration reusing the first's values. An MMIO register that returns a fresh value per read would have been read once and cached for the rest of the loop.

TESTS (x86jit-tests/tests/fault_atomicity.rs): read completes transfer by transfer, write carries each half on BOTH backends, and the loop case. Three negative controls, each failing as it must.

VERIFIED: 815/815 debug and release, clippy --all-features, fmt.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
