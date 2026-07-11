---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-11 17:55'
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
FOUND + FIXED via the extended tracer (committed 439109e): vzeroall left the low 128 bits (xmm) stale — it lifted identically to vzeroupper (upper-only clear). vzeroall must zero the WHOLE register file incl. xmm0-15. IrOp::VZeroUpperAll gained clear_low; interp+JIT honor it. HW-confirmed (native_vzeroall_clears_whole_register_matches_interp) + jit==interp test. Invisible to jit==interp (both wrong identically) — the hardware oracle caught it. Discovery: extended lockstep replay to capture ALL non-control-flow ops in a guest-addr window (vzeroall has no operands → skipped by the vector-only pass).

TRACER now captures: all vector ops (any addr) + all scalar/data ops in [X86JIT_LOCKSTEP_LO,HI). Options: X86JIT_LOCKSTEP_MAX (record cap), X86JIT_LOCKSTEP_FLAGS (defined-flag compare, noisy due to elision), X86JIT_NO_FLAG_ELISION (ruled out elision). Replay tolerates truncated trailing record.

RESULT after vzeroall fix: genrsa 2048 ADVANCED — no longer silently corrupts; now traps on an UNLIFTED op: vpermilpd xmm,[mem],imm (c4 e3 79 05 ...) at ~0x1bc7514. This is the next trap-and-fix (predicted in earlier notes). Need to lift vpermilpd + vpermilps (imm form, reg+mem, xmm+ymm); likely variable form too.

STILL OPEN (separate from genrsa): dgst-sha256-sign with the host 2048 key still yields a wrong (but deterministic) signature. The wide window [0x1d30000,0x1d90000) all-data-op replay is 100% bit-exact vs hardware (12M records) — so the dgst residual bug is OUTSIDE that window OR in a masked-EVEX op (k-register operands are rejected by the tracer filter AND the native stub can't init opmasks = a blind spot). v4 advertises AVX512F/BW/DQ/VL/CD (no IFMA); traced rsaz ops were AVX2 (ymm). Next for dgst: widen window further, or add masked-EVEX coverage (needs stub k-mask init), or trace program-wide vector ops (the original 5.2M pass skipped zero-operand + masked ops).
<!-- SECTION:NOTES:END -->
