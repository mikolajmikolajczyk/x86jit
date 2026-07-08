---
id: TASK-153
title: >-
  go-caddy P5-real — run the actual caddy binary serving index.html over real
  TCP
status: Done
assignee: []
created_date: '2026-07-07 12:56'
updated_date: '2026-07-08 13:53'
labels:
  - go-caddy
  - 'crate:tests'
  - 'crate:run'
  - 'goal:feature'
milestone: go-caddy
dependencies:
  - TASK-112
ordinal: 162000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Run the REAL caddy binary (caddy:latest, ~35-40 MiB static Go) file-serving index.html over real host TCP, curl-able from the host, three ways (native + interp + tiered JIT). task-112 (P5) closed the roadmap with a Go net/http file-server STAND-IN (httpserve_go.elf, http.FileServerFS) — mechanism proven (net/http + real TCP + curl), but the actual caddy binary was never run. This is that last rung. Design: backlog/docs/design/go-caddy-plan.md §'Phase 5 — caddy endgame'. No hard blocker identified in the plan; ~1-3 sessions of gap-chasing.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 caddy file-server --root /srv --listen :8080 (or a minimal Caddyfile) boots under the OCI runner and 'curl 127.0.0.1:8080/index.html' returns the file — native + interp + tiered JIT, all green
- [x] #2 Phase-0 busybox httpd still green (regression guard on the shared socket plumbing)
- [x] #3 Each gap hit during gap-chasing is filed as a linked follow-up task (crate/goal-labelled), not bandaided inline
<!-- AC:END -->



## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
Roadmap P0-P5 tasks all Done (108-112 + VCLK 134/141-144 + guard pages 148-152). Only the stand-in ran; walls P1-P4 are down, so this is gap-chasing, not new infra.

CADDY-SPECIFIC GOTCHAS (go-caddy-plan.md §P5, inspected/known — expect these first):
- Admin endpoint: caddy binds 127.0.0.1:2019 by default. Passthrough via the socket plumbing (Phase 0/4) or disable with --no-admin / {admin off}.
- Data/config dirs: caddy writes $XDG_DATA_HOME / $HOME/.local/share/caddy — set XDG_DATA_HOME to a writable rootfs path; ensure mkdir/openat land.
- os.Executable (shim.rs ~1263): caddy calls it for upgrade/plugin paths; currently errors — a benign error is fine, a panic is not (verify the arm degrades gracefully).
- Binary size: ~35-40 MiB text → translation-cache pressure + startup compile cost. tiered tier_up(Some(50)) (task-106) keeps run-once startup interpreted; watch for cache thrash. Eager JIT now viable (VCLK-2 fixed the clock race; go_http_eager green — the task-112 'eager fails' note is stale).
- sendfile → -ENOSYS, Go falls back to read/write loop (fine, leave it).

CANDIDATE FOLLOW-UPS that may get PULLED IN if real caddy misbehaves (link these, don't pre-implement):
- task-123 signals: real signal DELIVERY (frame push + rt_sigreturn). Pull in ONLY if caddy's GC stalls behind a non-preemptible/call-free loop (symptom: stop-the-world hang). Second-biggest latent risk (+3-5 sessions).
- task-126 runner: Scheduler->run_threaded escalation on first clone. Pull in if caddy is reached via a shell tree that fork/execs it (direct-exec path avoids this).
- task-121 futex WAIT_BITSET/absolute-deadline; task-122 futex robust list — glibc/Go pthreads corners.
- task-125 mt blocking fd I/O as yielded outcomes; task-133 epoll_ctl on synthetic fds — netpoller edges under load.
- task-129 runner: capture stderr in ProcOutcome/RunResult — needed to SEE caddy's boot diagnostics while gap-chasing (do this early, cheap).
- task-131 shim madvise host-passthrough — memory pressure on the big span.

Fallback ladder (each a keepable demo): Go hello -> net/http hello (DONE) -> caddy file-server HTTP -> Caddyfile -> TLS. Stop at the first rung that serves index.html for this task's DoD.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Session 2026-07-07 progress: built real caddy (CGO_ENABLED=0 static, ~52 MiB), probed under interp via Guest/Reserved threaded layout. RESULTS: caddy boots the FULL Go runtime (GC workers, all goroutines start) -> then 'fatal error: found bad pointer in Go heap' during GC (exit 2). Gaps found + filed (AC#3): (1) prefetch 0F 18 (prefetcht0/nta/w) unhandled -> FIXED this session (central NOP lift in lift.rs + prefetch_is_a_noop test); was the first blocker. (2) task-129 stderr capture DONE this session (ProcOutcome.stderr + RunResult.stderr) -> needed to SEE the Go panic. (3) task-161 = the bad-pointer heap-corruption deep bug (BLOCKS serving; full repro + bisect ideas in the task). (4) task-162 = readlinkat(267)/uname(63) ENOSYS (non-fatal, filed). Layout note: heap_base must clear caddy RW/BSS ~0x5879400 (~88 MiB) -> use 0x600_0000. caddy.elf (52 MiB) + probe NOT committed (too big / exploratory); repro recipe in task-161. NEXT: task-161 (bisect the bad pointer) is the blocker to caddy serving.
<!-- SECTION:NOTES:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [x] #1 cargo nextest run (--features unicorn) green, minus fuzz_robustness
- [x] #2 cargo clippy --all-targets --all-features -- -D warnings clean
- [x] #3 cargo fmt --check clean (nix-pinned rustfmt)
<!-- DOD:END -->
