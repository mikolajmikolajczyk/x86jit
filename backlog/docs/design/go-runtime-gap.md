---
id: doc-21
title: 'Running Go binaries (caddy) — the gap, and the achievable web-server path'
type: guide
created_date: '2026-07-06 11:25'
---

# Running Go binaries (caddy) — the gap, and the achievable web-server path

Status snapshot (2026-07-06). Empirical findings from running a real static Go
binary (`caddy:latest`, `/usr/bin/caddy version`) under the x86jit OCI runner.
Recorded here rather than as a GitHub issue so it survives an autonomous session;
promote to an issue if the Go track becomes a milestone.

## What happens today

caddy's ELF loads and executes; the Go runtime bootstraps through `rt0_go` →
`schedinit` → `mallocinit`, then dies:

```
fatal error: failed to reserve page summary memory
runtime.(*pageAlloc).sysInit  runtime/mpagealloc_64bit.go:81
runtime.(*pageAlloc).init     runtime/mpagealloc.go:327
runtime.(*mheap).init         runtime/mheap.go:821
runtime.mallocinit            runtime/malloc.go:493
runtime.schedinit             runtime/proc.go:877
```

Getting this far needed one new syscall — `sched_getaffinity` (204), added this
session (report one online CPU). Everything up to `mallocinit` works.

## Wall #1 — Go's virtual-memory model vs the Flat model (architectural)

Go's page allocator `sysReserve`s a very large **sparse** virtual arena
(hundreds of GB of `PROT_NONE`, committed lazily) plus a multi-MB page-summary
structure keyed on the full 48-bit address space. The x86jit `Flat { size }`
model is a single **eagerly-allocated** contiguous host buffer (128 MiB in the
runner) — it cannot answer a giant reservation, so `sysReserve` fails and Go
aborts before it ever creates a thread or touches the network.

This is the first and deepest wall. Fixing it is not a syscall patch; it needs
the **`SoftMmu`** memory model (spec §4.1, currently `todo!()`): a sparse
page-table address space where `mmap(PROT_NONE, huge)` reserves cheaply and pages
commit on fault. That is a real milestone, not an evening's work.

## Walls #2+ (never reached by caddy today, in dependency order)

1. **Threads** — `clone(CLONE_VM)` returns `-ENOSYS` (shim.rs). Go's M:N
   scheduler creates OS threads; needs the mt substrate (plan D4).
2. **Signals** — `sigaltstack`, `rt_sigaction`/`rt_sigreturn` machinery (Go
   installs a SIGURG-based preemption handler and a SIGSEGV handler).
3. **Networking** — zero socket syscalls exist (`socket`/`bind`/`listen`/
   `accept4`/`epoll_*`). A server cannot listen.

So "install caddy and serve index.html" is blocked behind SoftMmu + threads +
signals + a socket layer — multiple milestones, not a patch.

## The achievable web-server-on-x86jit path (no Go)

Serving `index.html` over real TCP is reachable **without** Go, via a program
whose model the engine already fits:

- **busybox `httpd`** (the musl busybox image already runs three ways) — a
  fork-per-connection server. `fork` is proven (OCI multiprocess). No threads,
  no Go VM reservation, no epoll — only **blocking sockets**.
- Needed: a minimal blocking-socket layer in the shim — a `Fd::Socket`
  variant bridged to host `std::net`, wiring `socket`/`setsockopt(SO_REUSEADDR)`/
  `bind`/`listen`/`accept`/`close`. Blocking `accept` blocks the emulator
  thread, which is fine for a demo. `curl` from the host is the test.

That is the honest version of the owner's "serve index.html on our JIT" goal and
a natural first phase.

## Full execution plan

**[`go-caddy-plan.md`](go-caddy-plan.md)** — the six-phase, dependency-ordered
roadmap from "Go aborts at mallocinit" to "caddy serves index.html over real TCP":
P0 blocking sockets + busybox httpd (the early curl-able win) → P1 `BigFlat`
(host `mmap(NORESERVE)` backing, not a page-table SoftMmu — keeps the one-add
translation) → P2 threads (`clone(CLONE_VM)` → host threads, promoting the proven
`mt.rs` recipe into the shim; the highest-risk phase) → P3 minimal signals
(sigaltstack is a P2 dependency — Go's asm crashes on its `-ENOSYS`) → P4 epoll
netpoller → P5 caddy. ~11–23 sessions, dominated by P2.
