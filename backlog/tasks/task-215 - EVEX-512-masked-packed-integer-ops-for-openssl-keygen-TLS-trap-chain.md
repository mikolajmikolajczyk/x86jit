---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-12 06:43'
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
CADDY HTTPS WORKS (both interp AND jit) under --cpu v4: real 52MB Go caddy binary runs 'caddy run' with a Caddyfile, terminates TLS with our cert, serves index.html over HTTPS to a host openssl s_client — 'HTTP/1.1 200 OK, Server: Caddy', body delivered over the encrypted connection. Needed: lift SSE4.1 pblendw (VBlendW, dst=src1, preserves upper); new syscalls mkdir(83)/mkdirat(258), rename(82)/renameat(264)/renameat2(316) (gated to writable passthrough dirs), sysinfo(99) (plausible mem/uptime). recvmmsg(299) ENOSYS is non-fatal (Go falls back). Caddyfile must use skip_install_trust + explicit tls cert/key (local_certs makes caddy install a root CA and exit). jit==interp test pblendw_match_interp; compat+ratchet updated. NEXT: s_client (guest as TLS client connecting out).
<!-- SECTION:NOTES:END -->
