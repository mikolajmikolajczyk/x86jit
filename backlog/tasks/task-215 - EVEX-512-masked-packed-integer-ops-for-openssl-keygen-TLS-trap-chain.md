---
id: TASK-215
title: EVEX-512 masked packed-integer ops for openssl keygen/TLS (trap chain)
status: In Progress
assignee: []
created_date: '2026-07-11 12:27'
updated_date: '2026-07-12 06:17'
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
TLS HANDSHAKE WORKS end-to-end (both interp AND jit) under --cpu v4 --entropy host. Guest openssl s_server + host openssl s_client => full TLSv1.3/1.2 handshake, cert exchanged (CN=localhost), decrypted GET, served -www HTTPS page 'HTTP/1.0 200 ok'. Method: trap-and-fix loop on the handshake. New ISA lifted this session: vpmullw/vpmulhw/vpmulhuw (MulLo16/MulHiU16/MulHiS16), vpmulld (VEX), pmuldq/vpmuldq (MulS32), vp{sll,srl,sra}v{w,d,q} (VShiftVar per-element variable shift), vp{sll,srl,sra}{d,q} v,v,xmm (VShiftReg scalar-reg count), vpsrlq/vpslld zmm,[mem],imm (mem-src imm shift), GFNI wide/masked vgf2p8affineqb/mulb on ymm/zmm incl rip-rel mem matrix (VGf2p8, reuses GfniOp::apply), vpblendvb/vblendvps/vblendvpd VEX 4-op (VPBlendVX), vpcmpeqq/vpcmpgtq->k, vextracti32x4/64x4 [mem],zmm,imm (VExtractLaneWideM). MulHi16 codegen: widen+imul+scalar-shr+byte-shuffle repack (umulhi/uunarrow unsupported in ISLE). New syscalls (x86jit-linux/shim.rs): sendto(44)/recvfrom(45)/sendmsg(46)/recvmsg(47) forward to host socket incl cmsg passthrough for KTLS; select(23)/pselect6(270) forward host-backed fds to host select, non-host always-ready; ioctl FIONBIO/FIONREAD on sockets. All jit==interp (jit tests packed_muls/variable_shifts/shift_imm_mem_src/gfni_wide/blend_and_cmpq/extract_lane_mem_dst_match_interp). compat map + coverage ratchet updated. clippy+fmt clean. Full nextest running.
<!-- SECTION:NOTES:END -->
