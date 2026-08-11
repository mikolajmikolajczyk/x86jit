---
id: TASK-323
title: >-
  Multi-vcpu soundness: invalidation races, shared-RAM aliasing, and SMC blind
  spots
status: To Do
assignee: []
created_date: '2026-08-11 11:02'
labels: []
dependencies: []
priority: high
ordinal: 359000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Five findings that are one property: what this engine guarantees when more than one vcpu runs over one Vm. Single-vcpu execution is unaffected by all of them, which is why none has ever visibly broken.

**Link/IBTC publication is not epoch-validated** (vm.rs ~1205 RET_LINK; codegen/mod.rs ~3690 chaining, ~3717 ibtc_or_miss, ~3832 return prediction). Reported independently by two reviews reading from opposite ends, which is why it leads. A vcpu can invalidate and clear every slot between resolve and publish; this side republishes the stale pointer and jumps to it. Keeping code bytes allocated prevents use-after-free — it does not prevent executing a translation of code the guest has since rewritten.

**Backing::as_mut_slice hands out &mut [u8] from &self** (memory.rs:170) on a type that is manually Sync and shared through Arc<Vm>. That is Rust aliasing UB whenever two accesses overlap, which is the normal case for a threaded guest. TSO barriers fix the guest's model; they cannot make the host program well-formed.

**helper_counters are non-atomic u64** (cranelift/src/lib.rs:1974) written by generated code from several vcpus and read concurrently by reporting. Accepted as 'lost updates make the diagnostic approximate'; the sharper objection is that it is a host data race, so the diagnostic is UB rather than imprecise.

**SMC tracking stops above CODE_WINDOW** (memory.rs:287). mark_code and note_write no-op past 4 GiB, and the comment beside them admits code above the boundary and blocks straddling it are valid. 'Guest code always lives low' holds for the current fixtures and is not a guarantee.

**Do compiled stores invalidate translated pages at all?** A review called this critical. Vm::unmap does invalidate (vm.rs:514, mirroring handle_smc), so the claim as stated is too broad — but the ordinary compiled-store path was never traced. Resolve it with a two-vcpu test where compiled code patches compiled code and immediately transfers into it, and record the result either way. If the stale block runs, this becomes the most urgent item in the backlog. Touches task-217's store path.

Merged from task-306, 307, 308, 309, 319.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Link, IBTC and return-predicted entries are epoch-validated immediately before transfer, or publication is serialised with invalidation
- [ ] #2 No &mut reference is created from a shared &self on guest RAM; the multi-vcpu tests run clean under Miri, or the reason they cannot is recorded
- [ ] #3 Helper counters are race-free and reporting uses atomic loads
- [ ] #4 SMC is tracked across the whole address space, or translation outside the tracked range is refused
- [ ] #5 The compiled-store SMC question is answered by a two-vcpu test and the answer is recorded
- [ ] #6 A deterministic test pauses between slot load and transfer and proves the stale translation cannot run
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
