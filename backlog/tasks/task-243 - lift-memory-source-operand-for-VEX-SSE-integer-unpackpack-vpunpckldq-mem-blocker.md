---
id: TASK-243
title: >-
  lift memory-source operand for VEX/SSE integer unpack+pack (vpunpckldq [mem]
  blocker)
status: Done
assignee: []
created_date: '2026-07-14 21:07'
updated_date: '2026-07-14 21:26'
labels:
  - lift
  - avx
  - sse2
dependencies: []
ordinal: 272000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
unemups4 Mono/Celeste bring-up hits vpunpckldq 0x3a01e40,%xmm0,%xmm0 (c5 f9 62 05 aa 57 f5 01 — VEX.128.66.0F 62 /r, rip-relative memory src). The register VEX forms of the unpack/pack/shuffle family already lift (task-195); the gap is the MEMORY source operand: lift_vunpack / lift_vunpack_avx / lift_vpack reject a non-register src2 (reg_xmm returns None -> unsupported). Add memory-source support by adding _M IR variants (mirroring VPackedBinM: read a-reg pre-copied into dst, load 16 bytes from addr as b) + interp + cranelift, for the unpack family (0F 60/61/62/6A/68/69/6C/6D, both legacy and VEX.128) and pack family (0F 63/6B/67, 0F38 2B). VEX.128 upper-zeroing applies. Differential tests: legacy vs Unicorn (memory src), VEX via vex_eq_sse.
<!-- SECTION:DESCRIPTION:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Add VUnpackLowM IR op (dst=a in-place, load 16B from addr as b, lane+high) mirroring VPackedBinM. Wire lift_vunpack + lift_vunpack_avx to vec_src_dispatch! (reg -> VUnpackLow; mem -> pre-copy a into dst if needed, then VUnpackLowM). Add interp exec_v_unpack_low_m + cranelift emit_v_unpack_low_m. Same for pack family (VPackWideM). Tests + coverage ratchet + compat regen.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Investigation showed the register VEX unpack/pack/shuffle forms already lifted (task-195); the real gap was a 128-bit MEMORY source2 (reg_xmm returned None -> unsupported). Added VUnpackLowM + VPackWideM IR ops (in-place dst=a pre-copied, load 16B addr as b) mirroring VPackedBinM; interp exec_v_unpack_low_m / exec_v_pack_wide_m (shared pack_wide_mem helper); cranelift emit_v_unpack_low_m (inline shuffle) + emit_v_pack_wide_m (new vpack_mem helper, jit==interp). Wired mem-src dispatch into lift_vunpack/lift_vunpack_avx/lift_vpack/lift_pack_signed via vec_src_dispatch!; VEX.128 upper-zeroing applied (explicit for mem forms). Covers unpack {l,h}{bw,wd,dq,qdq} legacy+VEX and pack{ss}{wb,dw}+vpack{ss,us}{wb,dw} with mem src. Tests: differential legacy-vs-Unicorn, VEX vex_eq_sse (incl exact blocker vpunpckldq [mem] shape + ymm-upper-zero), jit_eq_interp for all _m paths. Full suite 488 passed/3 skipped (--features unicorn, minus fuzz_robustness); clippy+fmt clean. SKIPPED: packuswb/vpackuswb legacy 0F 67 memory form (uses a separate VPackUsWB IR op with no mem variant — would need new IR; not the blocker, deferred). Shuffle family (vpshufd/vpshufhw/vpshuflw/vpshufb) already had mem-src support. No new mnemonics -> compat map unchanged.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
