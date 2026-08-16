---
id: TASK-348
title: 'jit: dppd_native skips the x86 NaN rule, a latent ARM-host divergence'
status: To Do
assignee: []
created_date: '2026-08-15 13:27'
labels:
  - code-review
dependencies: []
priority: medium
ordinal: 384000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`dpps_native` (codegen/vector.rs:579) routes every product through `sse_nan_result` and every add through `sse_fadd`. `dppd_native` (vector.rs:653-679) uses a bare `fmul` (656) and a bare `fadd` (668), while its doc-comment claims it is "bit-identical to the dppd helper (interp/mod.rs)" -- and that helper (interp/mod.rs:4137) puts BOTH the products and the sum through `sse_binop_f64`.

SDM Vol 1 §4.8.3.5, Table 4-8: "SNaN and QNaN -> SSE/SSE2/SSE3/SSE4.1/AVX -- First source operand (if this operand is an SNaN, it is converted to a QNaN)."

The file's own `x86_nan_from_src1` (codegen/mod.rs:3609-3623) says why that must be emitted explicitly: "AArch64's FPProcessNaN prefers a signalling NaN over the first operand, so the two disagree in exactly one case -- SRC1 quiet, SRC2 signalling", and "Emitting it everywhere makes the same IR carry the same meaning on both hosts." The same comment names a second, host-independent reason: Cranelift, like LLVM, may commute a commutative float op.

HONEST LIMIT ON CONFIRMATION: on an x86 host the raw mulpd already implements the SDM rule, so the tiers agree today. The probe confirmed agreement rather than falsifying it:

    dppd(QNaN_A, SNaN_B) imm=0x31  interp 0x...7ff80000000000aa  jit 0x...7ff80000000000aa  agree
    dpps(QNaN_A, SNaN_B) imm=0x11  interp 0x...7fc000aa          jit 0x...7fc000aa          agree

x86jit-cranelift links only the host ISA backend, so the aarch64 lowering could not be executed here. This is a latent ARM-host defect -- the expensive class, since ARM is the product host and its CI lane is manual.

Blast radius: exactly 2 of the 12 raw float-op sites in the two codegen files; every other site is wrapped or single-operand.

Adjacent, minor, same review: MemCtx.fault_size (jit_abi.rs:71-72) is documented "Out: access width in bytes" but checked_addr writes the unbacked TAIL width, and nothing in the workspace reads the field -- unmapped_exit() never consults it.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 dppd_native applies the same NaN rule as dpps_native, or its doc stops claiming bit-identity with the helper
- [ ] #2 A test pins the SRC1-quiet/SRC2-signalling case for dppd and runs on the aarch64 lane
- [ ] #3 MemCtx.fault_size either has a consumer or is removed; its doc matches what is written
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
