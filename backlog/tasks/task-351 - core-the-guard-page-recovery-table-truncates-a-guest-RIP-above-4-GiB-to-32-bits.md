---
id: TASK-351
title: >-
  core: the guard-page recovery table truncates a guest RIP above 4 GiB to 32
  bits
status: To Do
assignee: []
created_date: '2026-08-15 13:28'
labels:
  - code-review
dependencies: []
priority: high
ordinal: 387000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`Entry::table: *const (u32, u32)` (codemap.rs:42) and `lookup` returns `table[idx-1].1 as u64` (codemap.rs:132). The producing site justifies the width with a constant that no longer exists -- codegen/control.rs:7-12:

    // Guest code lives below the 4 GiB CODE_WINDOW, so the `u32` SourceLoc is lossless.
    self.builder.set_srcloc(ir::SourceLoc::new(*guest_addr as u32));

CODE_WINDOW was removed. memory.rs:310-323 keeps the name only 'to explain why it is gone' and states the reason: "'Guest code always lives low' held for the fixtures and was never a guarantee; a dynamic image mapped high breaks it outright." The SMC table was rebuilt over the whole guest span for exactly that case; codemap was not.

Probe: JIT-compile a block at guest 0x1_0000_2000 (host-RAM Reserved model, guest_base = 0x1_0000_0000), then look the host entry PC up:

    guest RIP was 0x100002000, codemap::lookup reported Some(0x2000)

The embedder's SIGSEGV-in-JIT'd-code recovery therefore reports a wrong guest RIP -- a wrong si_addr or fault RIP delivered to the guest, or a resume at the wrong instruction.

The engine's own SMC above 4 GiB is sound; that was verified separately at the same guest_base on both tiers.

Two coupled sites: the u32 field/return in codemap.rs and the SourceLoc::new(... as u32) in codegen/control.rs. set_srcloc is called from exactly one place.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 A guest RIP above 4 GiB round-trips through the codemap unchanged
- [ ] #2 A test compiles a block at a high guest_base and asserts lookup returns the full address
- [ ] #3 The stale CODE_WINDOW justification in codegen/control.rs is replaced by what actually bounds the value
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
