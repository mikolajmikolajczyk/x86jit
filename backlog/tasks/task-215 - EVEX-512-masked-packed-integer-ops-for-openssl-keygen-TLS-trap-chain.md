---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-12 06:57'
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
S_CLIENT WORKS (both interp AND jit): guest openssl s_client connects OUT to a host-native TLS server, completes the handshake, receives the server banner, and its encrypted app-data is decrypted host-side (bidirectional TLS, guest=client). Needed: memory-source wide GFNI with dst==src1 aliasing (vgf2p8affineqb ymm,ymm,[rip+matrix]) — added VGf2p8M + shared gf2p8_mem_run<M:StrMem> reading the matrix from guest memory (interp via Memory, JIT via RawStrMem mem-fault helper), so no scratch reg needed. Replaces the earlier load-into-dst lowering (which deferred dst==a). Cert 'not yet valid' is a deterministic-clock artifact (cert dated 2026), not a handshake failure. jit==interp test gfni_wide_match_interp extended with the dst==a mem case. TLS TRIFECTA COMPLETE: s_server (guest server), caddy HTTPS (real Go server), s_client (guest client) all work under --cpu v4 both backends.
<!-- SECTION:NOTES:END -->
