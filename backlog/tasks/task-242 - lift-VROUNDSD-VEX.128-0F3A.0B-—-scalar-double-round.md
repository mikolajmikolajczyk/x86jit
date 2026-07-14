---
id: TASK-242
title: lift VROUNDSD (VEX.128 0F3A.0B) — scalar double round
status: In Progress
assignee: []
created_date: '2026-07-14 20:44'
updated_date: '2026-07-14 21:00'
labels:
  - lift
  - avx
  - sse4
dependencies: []
priority: high
ordinal: 271000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 retail bring-up (Mono runtime / Celeste) hits an unimplemented lift: vroundsd $0x9,%xmm1,%xmm0,%xmm1. Faulting bytes: c4 e3 79 0b c9 09 (VEX.128.66.0F3A.WIG 0B /r ib, imm8=0x09). Mono's math/JIT uses ROUNDSD-family for Math.Round/Floor/Ceiling with explicit rounding mode (imm8 selects mode: bit3=1 -> use MXCSR, low bits = round mode; 0x9 = round toward -inf, suppress-precision). Also lift the sibling ROUNDSS/ROUNDPS/ROUNDPD (0F3A.08/09/0A/0B) while here — a managed runtime will exercise all four. Semantics: dst = round(src2) per imm8 mode, dst[127:64] from src1 (VROUNDSD keeps upper qword of the first operand). Target ISA is Jaguar/x86-64-v2 (SSE4.1 present), so these are in-scope. Blocks unemups4 FASE-2 managed-entry.
<!-- SECTION:DESCRIPTION:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Legacy round{ss,sd,ps,pd} + IrOp VPRound/exec/cranelift already exist (task-168.5.4); only VEX decode was missing. Add lift_vround (packed 2-op) + lift_vround_scalar (3-op, merge base = op1) modeled on lift_vrndscale; wire Vroundps/pd/ss/sd dispatch. Tests: legacy diff vs Unicorn (all 4 modes) + VEX vex_eq_sse + exact blocker vroundsd 0x09 + ymm-upper-zero. Register 4 VEX mnemonics in coverage ALLOWLIST + regen compat map.
<!-- SECTION:PLAN:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
