---
id: TASK-323
title: >-
  Multi-vcpu soundness: invalidation races, shared-RAM aliasing, and SMC blind
  spots
status: In Progress
assignee: []
created_date: '2026-08-11 11:02'
updated_date: '2026-08-13 18:30'
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
- [x] #1 Link, IBTC and return-predicted entries are epoch-validated immediately before transfer, or publication is serialised with invalidation
- [x] #2 No &mut reference is created from a shared &self on guest RAM; the multi-vcpu tests run clean under Miri, or the reason they cannot is recorded
- [x] #3 Helper counters are race-free and reporting uses atomic loads
- [x] #4 SMC is tracked across the whole address space, or translation outside the tracked range is refused
- [x] #5 The compiled-store SMC question is answered by a two-vcpu test and the answer is recorded
- [x] #6 A guest following the SDM Vol 3A §11.1.3 cross-modifying-code protocol (modifier stores code then raises a flag; executor polls, executes a serializing instruction, then runs the code) observes the NEW code across two vcpus — replacing the original AC#6, which asked for a pause between slot load and transfer
<!-- AC:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
AC#6 REPLACED, with the maintainer's agreement, and then met.

The original asked for a deterministic test pausing between a link slot's load and the transfer through it. Two things were wrong with it. The pause is inside GENERATED code, so forcing it needs a hook in emitted code that does not exist and should not be added for a test. And the property it demanded — a stale translation can never run — is stronger than the architecture grants. SDM Vol 3A §11.1.3 calls unsynchronized cross-modifying code MODEL-SPECIFIC ('IA-32 processors exhibit model-specific behavior when executing cross-modifying code, depending upon how far ahead of the executing processors current execution pointer the code has been modified') and puts a serializing instruction on the EXECUTING processor as part of the required protocol. Enforcing more than that would cost a load/compare/branch on every chain transfer — the path fast dispatch exists to keep free — and buy nothing a guest can rely on.

The replacement pins what a guest CAN rely on: the SDM protocol itself. `x86jit-tests/tests/cross_modifying.rs`, both backends. One vcpu runs the target (so a translation exists), raises READY; the other waits, stores the new code, raises Memory_Flag; the first polls, executes `cpuid`, calls the target again and must see the NEW value. Deterministic by construction rather than by luck — the flag handshake IS the synchronization, so there is no interleaving to win and nothing to make flaky.

What actually makes it pass is worth recording, because it is not the serializing instruction (this engine treats `cpuid` as an ordinary op): the compiled store marks the code page dirty (task-329) and the compiled inner loop leaves its chain as soon as any code page is dirty, so the polling vcpu reaches `handle_smc` before re-entering the target.

Both halves verified load-bearing by breaking each: stub out the store's code-page gate -> JIT case fails; remove the chain-leave -> JIT case fails. The INTERPRETER case survives the second, because it reaches `handle_smc` every block by construction — which is precisely why running this on one backend would have proved nothing about the other. That shape has now bitten this session three times.

VERIFIED: 821/821 debug and release, clippy --all-features, fmt.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
