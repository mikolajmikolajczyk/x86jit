---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-11 18:47'
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
SIGNING FIXED (committed dcf2872). Root cause: interp's exec_bswap did (v as u32).swap_bytes() for size!=8, so a 16-bit `movbe [mem],r16` byte-swapped 32 bits instead of 2 (real `bswap r16` is undefined; movbe needs a true 2-byte swap). openssl's PEM/base64 key decode uses 16-bit movbe -> corrupted private key -> invalid RSA signatures. Fix: swap exactly `size` bytes. INTERP-ONLY bug (Cranelift JIT reduced to I16 correctly) -> jit==interp never caught it (no 16-bit movbe test); the hardware oracle (lockstep tracer) found it: `movbe [r13+1],cx` diverged on ALL shards, interp wrote a half-zero word.
Also lifted vpermq/vpermpd (imm8, VEX.256) memory-source (RSA-1024 signing trapped there first).

VERIFIED: openssl dgst -sha256 -sign under --cpu v4 = byte-identical to host for RSA-1024 AND RSA-2048; host `dgst -verify` = "Verified OK". Combined with genrsa 2048 producing a valid key, the full RSA crypto stack (keygen + sign + verify) works under v4.

Tracer method that cracked it: capture ALL data ops program-wide (window=[0,max]) with a record cap; replay found the first hardware divergence at the movbe. Same approach found vzeroall. Both were interp/JIT-shared or interp-only bugs invisible to jit==interp; the native-CPU oracle is what mattered.

REMAINING toward TLS: run a real handshake (openssl s_server/s_client or caddy HTTPS). Tracer + fixes in place; next is an end-to-end TLS test.
<!-- SECTION:NOTES:END -->
