---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-11 18:04'
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
MILESTONE (committed f5ef822 + 439109e): openssl genrsa 2048 --cpu v4 now PRODUCES A VALID KEY (host `openssl rsa -check` = "RSA key ok"). Two fixes got there: (1) vzeroall zeros the whole vector register incl. low 128 (was upper-only); (2) lifted vpermilps/vpermilpd (imm8, VEX.128, reg+mem) -> VShuffle32. The full AVX2 rsaz keygen math is correct end-to-end. Found via the extended lockstep tracer (all-data-op capture in an address window; vzeroall has no operands so the vector-only pass had skipped it).

NEW, SEPARATE BUG (dgst-sign): `openssl dgst -sha256 -sign key2048.pem` still yields a WRONG signature (host verify: "invalid padding" — genuinely invalid, not just different). Characterized:
- DETERMINISTIC (stable across runs, both entropy modes) -> a math error, not a blinding/RNG issue.
- NOT AVX2-specific: fails even with OPENSSL_ia32cap=~0x0:~0x28 (AVX2+BMI1 off). => this is NOT the rsaz-avx2 path — a DIFFERENT, general RSA-signing bug in the CRT private path (m^d via mod p / mod q + Garner recombine), distinct from the keygen hunt.
- Signing-specific: keygen (shares rsaz mont-exp) works; the CRT private-key op does not. Prior-session note said RSA-1024 sign works / RSA-2048 fails -> the difference is 1024-bit CRT factors (mod p,q) vs 512-bit, OR the CRT recombination.
NEXT for dgst-sign: run the lockstep tracer on `dgst -sign` with NO address window (all data ops program-wide, capped) OR a window over the generic BN montgomery + CRT code (NOT 0x1d5xxxx which is rsaz-avx2 and proven clean). The tracer is ready; this is a fresh sub-hunt. Note: masked-EVEX ops remain a tracer blind spot (k-register operands rejected; native stub can't init opmasks) if the signing path uses AVX-512.
<!-- SECTION:NOTES:END -->
