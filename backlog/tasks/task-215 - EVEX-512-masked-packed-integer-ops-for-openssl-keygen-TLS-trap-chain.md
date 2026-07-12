---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: Done
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-12 10:38'
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
DONE. Full TLS trifecta under --cpu v4, both interp+jit, CI green x86_64+aarch64: (1) guest openssl s_server serves HTTPS to host s_client; (2) real caddy serves HTTPS; (3) guest openssl s_client connects out + exchanges app-data. ISA lifted: packed muls (pmullw/pmulhw/pmulhuw/vpmulld/pmuldq), variable+reg-count+mem-src shifts, GFNI wide reg+mem (incl dst==src1 via VGf2p8M), VEX blends, vpcmpeqq/gtq->k, vextracti[mem], pblendw. Syscalls: sendto/recvfrom/sendmsg/recvmsg(+cmsg), select/pselect6, ioctl FIONBIO, mkdir/mkdirat, rename/renameat/renameat2, sysinfo. Follow-ups filed: 218-224 (code-review fixes, all Done) + 217/220 (deferred).
<!-- SECTION:NOTES:END -->
