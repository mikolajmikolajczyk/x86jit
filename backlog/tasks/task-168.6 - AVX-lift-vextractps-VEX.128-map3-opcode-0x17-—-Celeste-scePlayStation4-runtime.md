---
id: TASK-168.6
title: >-
  AVX: lift vextractps (VEX.128 map3 opcode 0x17) — Celeste scePlayStation4
  runtime
status: Done
assignee: []
created_date: '2026-07-15 08:51'
updated_date: '2026-07-15 09:27'
labels:
  - m8-simd
dependencies: []
parent_task_id: TASK-168
ordinal: 277000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Retail bring-up (unemups4 Celeste, task-113.4.1) hit an unlifted AVX op in the scePlayStation4 managed-runtime interop module during boot, BEFORE any GNM graphics submit:

  unimplemented lift in x86jit for: vextractps $0x2,%xmm0,0x2c(%rsp)
  faulting bytes: c4 e3 79 17 44 24 2c 02
  (VEX 3-byte C4, map 0F3A (m-mmmm=3), opcode 0x17 = VEXTRACTPS; imm8 selects the 32-bit float lane, dst is r/m32 — here a memory store to 0x2c(%rsp))

VEXTRACTPS extracts one 32-bit float element (imm8[1:0]) from an XMM source to a GPR or memory dword. It is the VEX.128 form of SSE4.1 EXTRACTPS (66 0F 3A 17). Not covered by 168.1 (basic VEX.128 SSE ops) nor 168.3 (which does vextracti128 — a different, 128-bit-lane extract). AVX-128 only (no YMM state needed).

Acceptance: vextractps reg and mem dst forms lift + execute interp == jit == unicorn; the unemups4 Celeste boot advances past scePlayStation4 +0x57ab (rip 0x15cb476).
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
