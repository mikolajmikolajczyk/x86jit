---
id: TASK-197.5
title: 'MODE-A.5: 32-bit differential + fuzz coverage'
status: Done
assignee: []
created_date: '2026-07-10 10:32'
updated_date: '2026-07-10 12:45'
labels:
  - guest-modes
dependencies:
  - TASK-197.1
parent_task_id: TASK-197
ordinal: 226000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Wire the existing unicorn differential and fuzzer to run a Compat32 lane (unicorn UC_MODE_32). Reuse the 64-bit case tables where encodings are shared; add 32-bit-only cases (address wrap, 67h forms, 16-bit stack ops, inc/dec short forms 0x40-0x4F which are REX bytes in long mode). This is the safety net every other A subtask cites.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Differential harness runs a 32-bit lane vs unicorn UC_MODE_32
- [x] #2 Fuzzer generates and diffs Compat32 blocks (incl. 0x40-0x4F inc/dec forms)
- [x] #3 CI job (manual dispatch, per repo convention) covers the 32-bit lane
<!-- AC:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Session 2026-07-10. Landed on feat/mode-a-diff @ 45cb9a7. 32-bit (Compat32) differential lane + Compat32 fuzz wired vs unicorn UC_MODE_32. Mode is a parameter (UnicornOracle32 / InterpreterOracle32 / run_with_backend_mode / gen_mode), not a fork.

FILES: x86jit-tests/src/{unicorn,oracle,fuzz}.rs; x86jit-tests/tests/{differential32.rs(new),fuzz.rs}; .github/workflows/ci.yml.

VERIFY: full workspace 'cargo nextest --features x86jit-tests/unicorn -E not binary(fuzz_robustness)' = 431 passed / 4 skipped. clippy --workspace --all-targets clean; fmt clean. 64-bit suite unchanged (bit-identical, still 409-lane green within the 431).

LANE COUNTS (differential32.rs): 17 mode-neutral cases PASS un-ignored (add/sub/logic/mul/div, inc/dec 0x40-0x4F short forms incl. every-reg row + encoding assert, movzx/movsx, setcc, cmov, shift/rotate, SSE logic, lea, jcc loop, push/pop 32-bit, push/pop 16-bit, in-range 67h [bx], 4GiB base+disp address wrap). Compat32 fuzz: unicorn_matches_interp_32 (seeds 1..300) + jit_matches_interp_32 (seeds 1..600) both PASS.

KNOWN-GAPS (2, #[ignore]d, integration un-ignores after merge):
- addr16_override_67h_wrap_32  -> owner 197.2 (67h 16-bit effective-address wrap within 64 KiB: [bx+si] exceeding 0xFFFF must truncate to 16 bits). Currently FAILS when run.
- call_ret_32                  -> owner 197.3 (32-bit call/ret return-EIP width: interp pushes 8-byte return addr, UC_MODE_32 pushes 4; regs net-match after ret but leftover stack bytes diverge). Currently FAILS when run.

FINDINGS (mode-neutral truths, NOT gaps — un-ignored):
- 4 GiB base+disp address wrap already works: the effective_address seam truncates to 32 bits under Compat32 on pure 197.1 plumbing. Kept as live coverage (addr_wrap_4gib_32).
- push/pop with explicit 32-bit or 16-bit operands are mode-neutral (operand size, not mode, sets the byte count; stack pointer stays in range). The genuine 197.3 stack-width gap surfaces only in call/ret default width.
- inc/dec 0x40-0x4F short forms (REX bytes in long mode) decode+execute on plumbing alone (lifter already lifts Inc/Dec) — AC#2's headline case, un-ignored, with an assert that the encoding is genuinely the 1-byte form.

NO lifter/interp/codegen semantics were touched (197.2/197.3 own those). No plumbing bugs found. Fuzz register pool restricted to al/bl/cl/dl-addressable set (rax..rdx) because legacy 32-bit encoding has no sil/dil without REX.

FOR INTEGRATION: adding a case is data-driven via diff32(build, init, dont_care) in differential32.rs; port 197.2/197.3 local cases onto that helper and drop the #[ignore]. CI has a named '32-bit differential lane (MODE-A)' step (also covered by the whole-workspace run).
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
