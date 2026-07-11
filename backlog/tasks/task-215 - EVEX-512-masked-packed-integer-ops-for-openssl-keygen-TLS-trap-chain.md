---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-11 16:41'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 244000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
After task-214 unblocked openssl rand under --cpu v4, heavier crypto (openssl ecparam/genrsa/genpkey keygen, and full TLS) hits a CHAIN of unlifted EVEX-512 packed-integer ops. First trap: 'vpsrld zmm0,zmm0,0x1f' (62 f1 7d 48 72 d0 1f) — EVEX-512 packed shift-by-imm (we lift only VEX 128/256 via lift_vpacked_shift_avx + VPackedShift/VPackedShift256; no zmm/masked). Expect siblings: vpsll/vpsrl/vpsra{w,d,q} EVEX-512+masked, plus vpaddd/vpmuludq/vpshufd/vpand-family EVEX-512, vpternlog already done. Trap-and-fix under 'openssl genrsa 2048 --cpu v4 --entropy host' until keygen completes, then a real TLS handshake (openssl s_server/s_client or caddy HTTPS). Each op: extend the existing VEX lift to zmm + writemask via write_masked (masked-EVEX helper->interp pattern, task-209/214) + native bit-exact + jit==interp. HIGH VALUE: real TLS keygen end-to-end validates crypto through the full stack. Payoff of the task-211 crypto advertising + task-128 entropy work.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
FLAG-CHANNEL RESULT + ROOT-CAUSE REFINEMENT (this session, follow-up to the data-clean tracer runs):
Ran the replay with X86JIT_LOCKSTEP_FLAGS=1 and a per-op DEFINED-flag mask (iced rflags_undefined, so only architecturally-defined CF/PF/ZF/SF/OF are compared). Result: EVERY scalar op 'diverges' on flags immediately (add/sub/and/mul/imul/adc/neg all show interp flags != hardware). That is NOT a bug — it's the interpreter's DEAD-FLAG ELISION: when an op's flags are overwritten before any read, the lifter emits FlagMask::NONE and cpu.flags retains the previous LIVE value. So a post-op snapshot of cpu.flags does not equal that op's architectural flags; per-op flag comparison measures elision, not correctness.

CONSEQUENCE (tighter bug localization): a wrongly-elided (or wrongly-computed) flag that is CONSUMED by adc/sbb/adcx/adox would corrupt the GPR result -> the data pass (28.3M scalar ops, GPR+mem exact) is CLEAN, so all carry/borrow consumption is correct. The ONLY escape left for a flag bug is a flag consumed by a CONDITIONAL BRANCH (jcc/setcc/cmovcc) whose taken-direction our interp gets wrong -> wrong path -> wrong signature, with every data op still locally correct and invisible to per-op replay.

NEXT STEP (branch-point instrumentation, not per-op replay): in interpret_block, at each conditional branch on the rsaz path, capture (guest_addr, flag inputs, taken?) and replay just the compare+branch on hardware to verify the taken direction. First mismatch = the flag/branch bug. Alternatively: audit the lifter's flag-liveness/elision for the specific cmp/test/bt feeding jcc in rsaz_1024_avx2, and audit cmp/test (NOT in the scalar-arith capture set) + bt/bts flag production. Also still-open: the bug could be a non-arith op not captured (plain mov/load/store, cmp/test, bt) or outside the [0x1d50000,0x1d70000) window.

Committed: 7c81363 (tracer) + flag-mask refinement follow-up. Trace stays in /home/mikolaj/.cache/x86jit-lockstep.bin (regen via the repro under X86JIT_LOCKSTEP=<f> [+ _LO/_HI]).
<!-- SECTION:NOTES:END -->
