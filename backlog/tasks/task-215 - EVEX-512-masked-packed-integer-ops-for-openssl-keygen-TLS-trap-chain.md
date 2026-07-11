---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-11 14:59'
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
DONE (committed f4c7e74 + 1eae2e9; all native bit-exact + jit==interp + ratchet + full suite green, clippy/fmt/aarch64 clean):
- VMaskedShift: EVEX-512 masked packed shift-by-imm (vpsr/vpsl{d,q}, +vpsraq) any width, merge/zeroing.
- PackedBinOp::MulU32: pmuludq/vpmuludq unsigned 32x32->64, 128/256/512, reg+mem.
- VBlendD: vpblendd per-dword imm blend (128/256).
- VPerm1M: memory-source vpermq/vpermd (fault-capable helper; genrsa-1024 trap).
- vpbroadcastq zmm,xmm (EVEX-512 xmm-src broadcast; dgst-sign trap) via VToGpr+VBroadcastGpr.
WORKS under --cpu v4: genrsa 512 (full keygen), openssl rand (determinism+host entropy), dgst -sha256 (byte-identical to host), RSA sign RUNS.

BLOCKER (genrsa 2048 + sign): deep bug in openssl rsaz_1024_*_avx2 (used only for >=2048 keys => 1024-bit primes; hence 512/1024 keys work). Isolated via OPENSSL_ia32cap: AVX2-off (-e OPENSSL_ia32cap=~0x0:~0x28) => genrsa 2048 SUCCEEDS; AVX2-on => 'no prime candidate'. SHARED (interp AND jit both fail => not codegen). Every constituent AVX2 op proven bit-exact vs REAL HARDWARE (native_rsaz_avx2_battery, native_avx2_shift_all_counts, per-op native tests): vpmuludq(all widths,mem), vpaddq/d, vpsubq, vpsrlq/d @all counts, vpsllq, vpand/or/xor, vpermq, vpshufd, vpshufb, vpbroadcastq. => operand-specific edge in the COMPOSED rsaz routine, not a single-op bug. Op-level fuzzing exhausted; need openssl's REAL operands.

TRACER DESIGN (next step): lockstep interp-vs-native via IrOp::InsnStart snapshots. At each InsnStart{guest_addr}, cpu = that instruction's PRE-state; consecutive snapshots bracket one instruction (pre_i, post_i=pre_{i+1}). (1) Env-gate a capture in x86jit-core interpret_block: at InsnStart, if the finished instruction had a vector IrOp, record (guest_addr, xmm[0..16]+ymm_hi[0..16] pre & post, +64 bytes at any mem EA); ring-buffer, flush on Exit. (2) Replay harness in x86jit-tests: assemble [load pre; bytes; hlt], run_native, compare to captured post; first mismatch = the faulty op with real operands. (3) Run: dgst -sign with a host-valid 2048 key (hits rsaz_1024 with FAR fewer instructions than keygen's retry loop) under interp + capture; stop at first divergence. RSA-1024 (generic mont, no rsaz) works => confirms bug is rsaz-specific.

REMAINING clean-lift trap: vpermilpd xmm,[mem],imm (VEX.128 0x05; genrsa 1024 under some entropy). Likely vpermilps sibling too. Not yet lifted.
<!-- SECTION:NOTES:END -->
