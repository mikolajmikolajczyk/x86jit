---
id: TASK-184
title: >-
  oci run -it: interactive TTY + gap log + scripted-stdin CI driver (follows
  task-183)
status: To Do
assignee: []
created_date: '2026-07-09 12:38'
labels:
  - go-caddy
dependencies: []
ordinal: 208000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Follow-on to TASK-183 (registry pull + `oci run` landed). The remaining phases of the
docker-run-style experience: interactive TTY, a gap log, and a scripted-stdin CI driver.

## Done in TASK-183 (context)
- `x86jit-cli oci run [registry[:port]/]name[:tag|@digest] [-- CMD...]` pulls an image
  from a registry (native OCI-distribution client) into a temp rootfs and runs it.
- Content-addressed blob cache (`$X86JIT_OCI_CACHE`, CI `actions/cache`).
- All image tests converted to native pull; skopeo + committed tars gone.
- `-i` piped stdin works; `run_registry(...)` lib entry exists.

## P3 — Interactive `-it` (the hard part)
Give `x86jit-cli oci run -it <ref> -- /bin/sh` a real interactive shell.
- Guest side (x86jit-linux shim): emulate a tty on fds 0/1/2 — `ioctl`
  TCGETS/TCSETS(W)/TIOCGWINSZ/TIOCSWINSZ/TIOCGPGRP/TIOCSPGRP, `isatty()` true, plus the
  minimal job-control syscalls a shell issues (setpgid, tcsetpgrp/tcgetpgrp, SIGINT/
  SIGTSTP/SIGWINCH). Reuse the existing blocking-fd yield machinery for a blocking
  `read` on stdin (shim.rs ~475). Today unknown ioctls fall to -ENOSYS and `isatty()`
  reports false (shim.rs:1447).
  - DECISION: faithful ioctl-shim (fake termios state) vs allocate a real host pty pair
    and proxy. Real pty is more correct (line discipline, echo, ^C); the ioctl-shim is
    portable but must emulate line discipline. Evaluate.
- Host side (CLI): raw-mode terminal, forward host stdin → guest fd 0, guest fd 1/2 →
  host tty, SIGWINCH → guest TIOCGWINSZ updates, restore termios on exit.

## P4 — Gap log
- Generalize the existing `(gap:syscall)` convention (shim.rs logs unhandled syscalls
  once → -ENOSYS) to `gap:syscall` / `gap:ioctl` / `gap:insn`, each logging number/name
  + guest RIP once (dedup by key). A `--log-gaps` flag (or verbose default under
  `oci run`) prints them, and a structured summary at exit ("image needed: ioctl
  TIOCSPGRP, syscall statx, …") — the "what's missing" report.
- Route the lifter's `LiftError::Unsupported` into the same log (insn bytes + mnemonic +
  RIP) instead of only trapping out.
- Note already observed: `oci run alpine … cat /etc/os-release` emits
  `gap:syscall 40` (sendfile) before falling back.

## P5 — Fuller CI / programmatic harness
- A scripted-stdin driver over `run_registry` so CI can drive an interactive session
  non-interactively (feed input bytes, capture output, assert) and read back the gap
  summary.
- CI test: a scripted `/bin/sh` session against a digest-pinned public image asserts
  output + that the gap set is empty (or a known expected set) — turning "does a real
  distro shell work" into a programmatic regression. (`registry_run.rs` is the seed.)

## Verify
Each phase: cargo build + clippy + full suite green. End-to-end: `x86jit-cli oci run -it
<registry>/busybox -- /bin/sh` gives an interactive shell; a scripted CI run asserts
output + gaps.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 -it gives an interactive shell: tty ioctl emulation in the shim + host raw-mode driver + winsize/stdin forwarding
- [ ] #2 gap log surfaces unimplemented syscall/ioctl/insn (number/name + RIP), summarized at exit
- [ ] #3 scripted-stdin driver over run_registry so CI can pull-run-assert an interactive session programmatically
- [ ] #4 CI test: digest-pinned image, scripted shell session asserts output + gap set (skip-on-no-network)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
