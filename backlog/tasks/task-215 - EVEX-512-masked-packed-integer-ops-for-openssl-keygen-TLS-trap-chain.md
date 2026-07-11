---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-11 14:47'
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
Session 2026-07-11: lifted the EVEX-512/AVX trap chain for openssl v4 crypto. DONE (all native bit-exact + jit==interp + ratchet + 307 tests green, clippy/fmt/aarch64 clean):
- VMaskedShift: EVEX-512 masked packed shift-by-imm (vpsrld/vpslld/vpsrad/vpsrlq/vpsllq/vpsraq zmm + writemask). Added missing Vpsraq dispatch (AVX-512-only).
- PackedBinOp::MulU32: pmuludq/vpmuludq unsigned 32x32->64, all widths (128/256/512) reg+mem.
- VBlendD: vpblendd per-dword imm blend (128/256).
- VPerm1M: memory-source vpermq/vpermd (fault-capable helper, the genrsa-1024 trap).
- vpbroadcastq zmm,xmm (EVEX-512 xmm-source broadcast, the dgst-sign trap) via VToGpr+VBroadcastGpr.
WORKS NOW: genrsa 512 full keygen; openssl rand (byte-identical determinism + host entropy); dgst -sha256 (matches host exactly); RSA signing runs (256-byte sig produced).

BLOCKER (genrsa 2048 + sign-verify): DEEP SHARED bug in openssl's rsaz_1024_*_avx2 path (only used for >=2048-bit keys => 1024-bit primes; explains why 512/1024 keys work). Isolated via OPENSSL_ia32cap masking: genrsa 2048 SUCCEEDS with AVX2 disabled (-e OPENSSL_ia32cap=~0x0:~0x28), FAILS 'no prime candidate' with AVX2 on. Confirmed SHARED (interp AND jit both fail => not a codegen bug). BUT every constituent AVX2 op is proven bit-exact vs REAL HARDWARE over fuzzed vectors (native_rsaz_avx2_battery + vpmuludq/vpblendd/vpermq/masked_shift native tests): vpmuludq, vpaddq/d, vpsubq, vpsrlq/d, vpsllq, vpand, vpor, vpxor, vpermq, vpshufd, vpshufb, vpbroadcastq. Signature fails verify identically under AVX2 on/off (sig_on==sig_off) though key is host-valid and SHA-256 is correct => the RSA private mod-exp result is wrong, but NOT AVX2-differential. Needs INSTRUCTION-LEVEL interp-vs-native trace infra (not present in repo) to pinpoint the rare-operand/composition edge. NOT an op-level bug findable by fuzzing.
REMAINING TRAP CHAIN (clean lifts, entropy-dependent): vpermilpd xmm,[mem],imm (VEX.128 0x05) hit by genrsa 1024 under some entropy; likely vpermilps sibling. NOT yet lifted.
<!-- SECTION:NOTES:END -->
