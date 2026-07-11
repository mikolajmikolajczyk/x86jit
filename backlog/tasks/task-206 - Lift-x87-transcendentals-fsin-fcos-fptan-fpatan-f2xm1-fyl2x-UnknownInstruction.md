---
id: TASK-206
title: >-
  Lift x87 transcendentals: fsin/fcos/fptan/fpatan/f2xm1/fyl2x
  (UnknownInstruction)
status: Done
assignee: []
created_date: '2026-07-10 22:14'
updated_date: '2026-07-11 09:08'
labels:
  - 'crate:core'
  - 'goal:isa-coverage'
dependencies: []
ordinal: 235000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Discovered while deepening the x87 differential (task-188): the interpreter does NOT lift the x87 transcendentals — fsin/fcos/fptan/fpatan/f2xm1/fyl2x/fyl2xp1/fsincos all trap UnknownInstruction. Present in real binaries via libm's x87 fallback paths. Needs F80 (80-bit) implementations (f80.rs) matching x87 semantics + reduction; validate against a documented reference (Unicorn's QEMU x87 transcendentals are not Intel-exact, so use a bounded-ULP guard vs libm, per the task-188 harness). Lower priority than SIMD gaps but a real ISA hole. task-188 left a tripwire test that fails when these get implemented, prompting a real differential.
<!-- SECTION:DESCRIPTION:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
DONE 2026-07-11. Lifted 8 x87 transcendentals (fsin/fcos/fptan/fpatan/f2xm1/fyl2x/fyl2xp1/fsincos). Not bit-exact-able to Intel → f64/libm precision, validated bounded-ULP. Precision selectable per maintainer (start f64/libm fast, F80 high-prec future option) — isolated behind F80::{sin,cos,tan,exp2m1,atan2,ylog2x,ylog2xp1} (f64-backed) so future F80 impl+knob is localized; no knob built yet per 'start simpler'. FpuKind variants + exec_x87 cases (correct stack push/pop, reduction-domain guard |ST0|>=2^63 leaves operand, C2 not modeled). Routing: added to lift/mod.rs:612 lift_x87 allow-list (else whole block fails to lift) + lift/control.rs dispatch. JIT: zero new plumbing (reuses x87 helper→exec_x87). Native oracle can't capture x87 → validation is interp-vs-Unicorn(+libm); upgraded task-188 tripwire → x87_transcendentals_interp_within_ulp_of_libm (BOUND=2). differential.rs is #![cfg(feature=unicorn)]. 8 ratchet ALLOWLIST entries; coverage x87 lifted 37→45. Suite 542/542 (--features unicorn), clippy+fmt clean, aarch64 clean.
<!-- SECTION:NOTES:END -->
