---
id: TASK-203
title: >-
  VEX 3-operand op2==dst aliasing: pre-VMov clobbers source
  (VPshufb/VAlignr/vround/vsqrt/vmovss-sd)
status: In Progress
assignee: []
created_date: '2026-07-10 18:07'
updated_date: '2026-07-10 19:00'
labels:
  - 'crate:core'
  - 'goal:bug'
dependencies: []
ordinal: 232000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Sibling of task-202. Several VEX.128 3-operand lifts do an unconditional 'VMov dst<-op1' then reuse a 2-operand in-place IR op whose second source is a REGISTER (op2). When op2 aliases dst (op2==dst && dst!=op1), the VMov clobbers op2 before it is read, so the op computes from op1 instead of the original op2. Same root cause as the task-202 vaddsd bug (float(2**30)=0.0), which was fixed in lift_vfloat_bin by passing op1/op2 straight to the 3-operand VFloatBin (no pre-copy).

BROKEN SITES (lift.rs, VEX.128 register-op2 form, op2==dst hazard):
  - VPshufb ~1106  (vpshufb xmm0,xmm1,xmm0)
  - VAlignr ~1234  (vpalignr xmm0,xmm1,xmm0,imm)
  - VPRound ~2942  (vroundsd/ss xmm0,xmm1,xmm0,imm)
  - VFloatUnary ~4372 (vsqrtsd/ss xmm0,xmm1,xmm0)
  - VFloatMov ~4054  (vmovsd/ss xmm0,xmm1,xmm0)

ALREADY SAFE (reference for the fix): lift_vlogic_vex / lift_vpacked_bin_vex use a genuine 3-operand IR (VLogic/VPackedBin {dst,a,b}) for the register case and only VMov in the memory branch; lift_vcvt_scalar lowers op2 into a temp via read_scalar_float BEFORE the VMov; SSE 2-operand forms have no op1 so no VMov. lift_vfloat_bin (task-202) now matches this pattern.

FIX (altitude-correct): make each in-place IR op non-destructive by carrying an explicit source, like VFloatBin's {dst,a,b}: read both sources into locals before writing dst, so aliasing is safe. Touches ir.rs + interp.rs + cranelift/codegen.rs per op. Alternatively guard only the op2==dst case, but the general 3-operand form is cleaner and cheaper to reason about.

IMPACT: latent — no real binary observed to trap here yet (compilers usually emit op2!=dst). But these are valid encodings (merge/replace-upper idioms, in-place shuffle) and silently produce wrong results. Discovered while fixing task-202.

AC: (1) each broken site produces jit==interp AND native-correct output for the op2==dst form; (2) native cross-check test per op (dst==src2); (3) suite green + clippy + fmt.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->



## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
FIXED 2026-07-10. Generalized 5 in-place IR ops to carry an explicit source register (like VFloatBin's {dst,a,b}), so the lift no longer pre-copies op1 into dst — which clobbered a register op2 aliasing dst. Ops + new field 'a' (the value formerly read from dst): VPshufb (a=data), VAlignr (a=high/op1), VPRound (a=merge base), VFloatUnary (a=merge base), VFloatMov (a=upper-bytes source). Each touched ir.rs def + interp.rs exec + cranelift/codegen.rs + both lift call sites (SSE in-place passes a=dst; VEX 3-operand passes a=op1, VMov kept only in the memory branch where op2 can't alias). Callers affected: VPRound is used by SSE round{ss,sd} (lift_round) AND EVEX vrndscale{ss,sd} (lift_vrndscale) — the latter is the actual 3-operand aliasing site (VEX vroundsd is NOT lifted at all; noted below). VFloatUnary/VFloatMov by SSE sqrt/movss-sd + their VEX forms. TESTS: jit.rs vex_alias_family_dst_aliases_src2_match_interp (jit==interp: vpshufb/vpalignr/vrndscalesd/vsqrtsd/vmovsd, all op2==dst) + native.rs native_vex_alias_family_matches_interp (real CPU: the 4 AVX ops) + native_vrndscale_alias_matches_interp (real CPU: EVEX round). vmovsd 3-op reg form has no iced assembler method -> emitted via .db([c5 f3 10 ed]). NATIVE CAUGHT a false-pass: my first jit test used VEX 'vroundsd' which is UNLIFTED -> UnknownInstruction traps the whole block in BOTH jit and interp (identical -> jit==interp passes falsely); the native oracle exposed it (native ran, interp trapped). Replaced with the lifted EVEX vrndscalesd. SIDE NOTE: VEX vroundss/vroundsd (SSE4.1 round, VEX-encoded) are not dispatched in lift.rs — only SSE Roundss/Roundsd and EVEX Vrndscale*. Not observed trapping in real binaries; leaving unlifted (demand-driven).
<!-- SECTION:NOTES:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each of VPshufb/VAlignr/VPRound/VFloatUnary/VFloatMov carries an explicit source; register op2==dst no longer clobbered by a pre-copy
- [x] #2 jit==interp AND native-correct output for the op2==dst form of each op
- [x] #3 native cross-check tests (AVX family + EVEX vrndscale)
<!-- AC:END -->
