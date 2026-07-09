---
id: TASK-183
title: >-
  oci run -it: docker-run-style interactive image execution + registry pull +
  gap log (CI driver)
status: To Do
assignee: []
created_date: '2026-07-09 11:19'
labels:
  - go-caddy
dependencies: []
ordinal: 207000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
`docker run`-style interactive image execution through the recompiler:

    x86jit-cli oci run -it <registry[:port]/name:tag> [-- CMD...]

Pull the image from a registry into a temp rootfs, run it (optionally interactive, `-it`), and when the guest hits something unimplemented, surface it loudly in a gap log instead of dying silently. Doubles as a programmatic CI driver: pull-and-run any public image and assert on output + collected gaps.

DEPENDS ON: TASK-182 (the `x86jit-cli oci ...` subcommand must exist first). Prior context: decision-10 / task-167 (digest-pinned registry pull already exercised test-side via skopeo).

## Where we are today
- Registry pull exists ONLY in tests, via external `skopeo copy … docker-archive:` → a `docker save` tar → `x86jit-oci::load_image` (cached under target/oci-pull-cache/). No in-crate distribution client; no HTTP deps.
- `load_image` takes a LOCAL tar only.
- No ioctl / tty / pty / termios: unknown ioctls fall through to -ENOSYS, `isatty()` reports false (shim.rs:1447). Interactive bash can't run.
- A `(gap:syscall)` logging convention already exists: unhandled syscalls log once via eprintln (shim.rs:2364) then return -ENOSYS. This is the seed for the diagnostic mode.

## Target UX
    x86jit-cli oci run [-it] [--backend interp|jit] [--rm] [--log-gaps] <REF> [-- CMD ARGS...]
- `<REF>` = `registry[:port]/name:tag` (or `@sha256:…`) — parsed, pulled into a tmp rootfs (removed on exit unless kept).
- `-i` wire host stdin to the guest; `-t` allocate a tty (both = `-it`, interactive shell).
- `-- CMD...` overrides the image entrypoint (like `docker run … /bin/bash`).
- On any unimplemented syscall/ioctl/instruction → a clear line in the gap log (number/name + guest RIP), so "poke at the image, see what's missing" is the workflow.

## Phased plan (split into subtasks when scheduled)

### P1 — Registry pull into a tmp rootfs
- Parse `registry[:port]/name:tag[@digest]` (host:port, repo, tag/digest; default to docker.io if bare — decide).
- DECISION: native OCI-distribution client (HTTP: `/v2/` manifest + config + layer blobs, bearer-token auth, manifest v2 / OCI index, amd64 select) vs shell out to `skopeo` (reuse today's path, zero new deps, external requirement). Native = self-contained (best for `docker run` feel + CI without skopeo) but adds an HTTP client dep (ureq/reqwest) + auth. Recommend native, with skopeo as an interim fallback. Record as a decision.
- Extract layers into a tmp dir (reuse the existing tar/gzip layer machinery); `--rm` cleans it up.

### P2 — `oci run` subcommand (non-interactive first)
- Wire the pulled rootfs + config through the existing run_* machinery; `-- CMD` swaps the entrypoint (already supported by the argv-override path used in registry_pull.rs).
- No tty: stdin piped (`-i` only), stdout/stderr captured/streamed. This alone gives `x86jit-cli oci run <ref> -- /bin/busybox echo hi`.

### P3 — Interactive `-it` (the hard part)
- Guest side (x86jit-linux shim): emulate a tty on fds 0/1/2 — `ioctl` TCGETS/TCSETS(W)/TIOCGWINSZ/TIOCSWINSZ/TIOCGPGRP/TIOCSPGRP, `isatty()` true, plus the minimal job-control syscalls bash issues (setpgid, tcsetpgrp/tcgetpgrp, signal handling for SIGINT/SIGTSTP/SIGWINCH). Reuse the existing blocking-fd yield machinery (shim.rs:475) for a blocking `read` on stdin.
  - DECISION: faithful ioctl-shim (fake termios state) vs allocate a real host pty pair and proxy. A real pty is more correct (line discipline, echo, ^C) but couples the shim to a host pty; the ioctl-shim is portable but must emulate line discipline. Evaluate.
- Host side (CLI): put the terminal in raw mode, forward host stdin → guest fd 0, guest fd 1/2 → host tty, translate SIGWINCH → guest TIOCGWINSZ updates, restore termios on exit.

### P4 — Diagnostic gap log
- Generalize the `(gap:…)` convention to `gap:syscall` / `gap:ioctl` / `gap:insn`, each logging number/name + guest RIP once (dedup by key). A `--log-gaps` flag (or verbose default under `oci run`) prints them; collect into a structured summary at exit ("image needed: ioctl TIOCSPGRP, syscall statx, …") — the "what's missing" report the user wants.
- The lifter's `LiftError::Unsupported` path should feed the same log (insn bytes + mnemonic + RIP) rather than only trapping out.

### P5 — CI / programmatic harness
- A lib entry: `run_registry(ref, RunOptions{argv, stdin, interactive, …}) -> RunResult` plus a scripted-stdin driver so CI can drive an interactive session non-interactively (feed input bytes, capture output, assert) and read back the gap summary.
- CI test: pull a small public image (busybox/alpine, digest-pinned, skip-on-no-network like registry_pull.rs) and assert a scripted `/bin/sh` session's output + that the gap set is empty (or an expected known set). This turns "does a real distro shell work" into a programmatic regression.

## Verify
Each phase: cargo build + clippy + full suite green. End-to-end: `x86jit-cli oci run -it <registry>/busybox:latest -- /bin/sh` gives an interactive shell; a scripted CI run of the same asserts output + gaps. No regression to the existing tar-based `oci` path.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 x86jit-cli oci run [-it] <registry-ref> [-- CMD...] pulls to a tmp rootfs and runs it
- [ ] #2 registry ref parsing + pull into tmp (native OCI-distribution client or skopeo — decision recorded)
- [ ] #3 -it gives an interactive shell: tty ioctl emulation in the shim + host raw-mode driver + winsize/stdin forwarding
- [ ] #4 gap log surfaces unimplemented syscall/ioctl/insn (number/name + RIP), summarized at exit
- [ ] #5 lib entry run_registry(...) + scripted-stdin driver so CI can pull-run-assert programmatically
- [ ] #6 CI test: digest-pinned public image, scripted shell session asserts output + gap set (skip-on-no-network)
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [ ] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [ ] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
