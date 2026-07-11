---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-11 16:38'
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
LOCKSTEP TRACER BUILT + RUN (this session). Env-gated capture in x86jit-core/src/lockstep.rs (X86JIT_LOCKSTEP=<file>) hooked into interpret_block at each InsnStart; consecutive InsnStarts bracket one instruction (post_i==pre_{i+1}; a vector op is never a block's last op since blocks end AT control flow). Records full architectural side-state (gpr[16], flags, 64B mem-operand window, ymm0-15) pre+post per instruction. Replay harness = native::tests::replay_lockstep_trace (#[ignore]); mmaps the trace, re-runs each op on real host CPU via run_native from the captured pre-state, compares post GPR+mem+vec (flags opt-in via X86JIT_LOCKSTEP_FLAGS). Sharded: X86JIT_LOCKSTEP_SHARDS/_SHARD (N processes each own the fixed native VAs). Address window: X86JIT_LOCKSTEP_LO/_HI (hex) restricts capture + enables scalar-arith capture. Extended native stub (native.rs) to load YMM upper halves via vinsertf128 (was xmm-only). Repro: openssl dgst -sha256 -sign key2048.pem (hits rsaz_1024_avx2, far fewer insns than keygen); run under --backend interp --cpu v4 --entropy host; sig differs from host = bug reproduced under interp.

DECISIVE RESULTS (bug NOT found yet, but massively narrowed):
1. ALL 5.2M unique VECTOR ops (reg-only + memory-source), full arch effect (regs+gpr+flags+mem) = BIT-EXACT vs real hardware. Zero divergence. Every vector data-op, store, GPR<->vec transfer, and flag-setting compare is correct.
2. ALL ~28.3M unique SCALAR-ARITH ops (mul/mulx/adc/adcx/adox/add/sub/sbb/shl*/and/or/xor/lea/neg/...) in the rsaz window [0x1d50000,0x1d70000) = BIT-EXACT DATA (GPR + memory) vs real hardware. Zero data divergence (~35k/shard skipped = code/operand page VA collisions, ~1.5%).

=> The bug corrupts NO individual instruction's DATA result on the rsaz path. Remaining unverified channel = FLAGS (disabled: undefined-flag values differ from a specific host CPU's undefined result = noise; interp AND/adc flag SOURCE confirmed correct). Strong hypothesis: a DEFINED-flag -> wrong-conditional-branch divergence (interp executes a self-consistent but WRONG instruction sequence; per-op replay from captured pre-state can't see a wrong branch). OR the buggy op is OUTSIDE the rsaz window (e.g. the generic mont-exp caller), though that's shared with the working non-avx2 path.

NEXT STEP: re-run with X86JIT_LOCKSTEP_FLAGS=1 BUT add a per-instruction DEFINED-flag mask (iced rflags_written) so only architecturally-defined flags are compared -> first defined-flag divergence = the flag->branch bug. If clean, widen the address window / trace the mont-exp caller. Trace files land in /home/mikolaj/.cache (40GB for the scalar window; /tmp is tmpfs-small). Refactor pending: replay forks per op (~3000/s warm); a batched single-fork replayer would cut the 28M-op pass from ~40min.
<!-- SECTION:NOTES:END -->
