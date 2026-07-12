---
id: TASK-226
title: >-
  apps: v4 real-binary gaps (bzip2 file-output EINVAL, openssl speed
  alarm/times, zstd threads)
status: To Do
assignee: []
created_date: '2026-07-12 13:19'
labels:
  - 'crate:linux'
  - 'goal:app-coverage'
dependencies: []
ordinal: 255000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Trap-and-fix exploration of real x86-64-v4 binaries (session after task-215 TLS). VALIDATED bit-exact vs hardware under --cpu v4 JIT: openssl enc -aes-256-cbc (encrypt+decrypt roundtrip, host decrypts guest output); bzip2 compression (via -c stdout, host bunzip2 decompresses guest output to original). GAPS found (each its own follow-up): (1) bzip2 -f writing to an output FILE fails 'Can't create output file: Invalid argument' (EINVAL) — compression itself is fine (stdout works), so it's a post-open attribute syscall during output setup (likely fchmod/fchown/futimens/utimensat on the new file returning EINVAL); needs a syscall trace to pin, then implement/ignore that attr call. (2) openssl speed hangs/fails: needs alarm(37) + SIGALRM delivery (benchmark timing) and times(100) — depends on real signal delivery (see task-123). (3) zstd needs an I/O thread pool: clone3(435) + clone(CLONE_VM) -> the CLI single-process path returns ENOSYS; needs the mt-substrate auto-escalation (task-126). Low priority; the crypto/compression ISA is proven correct — these are syscall/OS-emulation gaps, not lifter gaps.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 bzip2 -f writes a valid output file (host bunzip2 decompresses it)
- [ ] #2 the failing bzip2 output-setup syscall is identified and handled (or safely no-op'd)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
