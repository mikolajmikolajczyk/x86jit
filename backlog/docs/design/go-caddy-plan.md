---
id: doc-23
title: 'Go-on-x86jit plan — caddy serving index.html over real TCP'
type: specification
created_date: '2026-07-06 11:25'
---

# Go-on-x86jit plan — caddy serving index.html over real TCP

Execution roadmap from "Go aborts at `mallocinit`" to "**caddy serves
`index.html` on a real TCP port; `curl` from the host returns it**".
Companion to [`go-runtime-gap.md`](go-runtime-gap.md) (the empirical walls).
Every claim below was re-verified against the tree at commit `504e97d`.

**End goal:** `caddy file-server --root /srv --listen :8080` inside the
`caddy:latest` rootfs under the OCI runner; `curl 127.0.0.1:8080/index.html`
from the host returns the file. Both engines (interp + JIT).

## Progress

- **Phase 0 (sockets + httpd) — DONE** (commit `43f7a63`). Shim forwards the socket
  family to real host fds (`Fd::Socket`); a freestanding server (`tcpserve.elf`) is
  reachable from the host via `std::net::TcpStream` under interp + JIT
  (`x86jit-tests/tests/socket.rs`). busybox-httpd-over-real-TCP with fork-per-conn is
  deferred — the deferred-fork scheduler can't serve an accepted connection
  concurrently, so that rung wants Phase 2 threads.
- **Phase 1a (Reserved model) — DONE** (commit `93196c2`). `MemoryModel::Reserved`
  + embedder-provided `MAP_NORESERVE` backing (ADR-0001). 512 GiB reserved, RSS
  < 20 MiB. Core stays `{iced-x86}`.
- **Phase 1b (runner rewire to the big span) — TODO.** Switch the OCI runner's arena
  to a `Reserved` span (via `Vm::with_backend_host_ram` + `hostmem::reserve`) behind a
  per-image heuristic; observe Go's abort move past `mallocinit`. Wants a Go fixture
  (and really Phase 2 to reach the next wall).

---

## Verified ground truth (what the code actually does today)

| Fact | Site |
|---|---|
| Flat model = one eagerly-allocated `vec![0u8; size]` | `x86jit-core/src/memory.rs:123` |
| JIT/interp translation is `host_base + guest_addr` (one add), base baked into `MemCtx` | `memory.rs:239-242`, `jit_abi.rs:56,198` |
| `SoftMmu` is `todo!()` and its backing is `Box::new([])` | `memory.rs:307,124` |
| Runner allocates 128 MiB; mmap arena tops out below an 8 MiB stack at `0x7f0_0000` | `x86jit-run/src/lib.rs:80-90` |
| `fork` deep-copies the *entire* backing buffer | `memory.rs::deep_copy` (~:141), `vm.rs:147` |
| SMC tracking = `Box<[AtomicBool]>`, one bool per 4 KiB page of backing | `memory.rs:104-129` |
| Guest `mmap` = bump arena; `PROT_NONE` reserve beyond the arena → `-ENOMEM` (this is the Go abort) | `shim.rs:843-894` |
| `munmap`/`mprotect` are no-ops returning 0 | `shim.rs:895-900` |
| `clone(CLONE_VM)` → `-ENOSYS` (deliberate; "use mt substrate, plan D4") | `shim.rs:1356-1374` |
| `futex` never blocks: WAIT → `-EAGAIN`, WAKE → 0 | `shim.rs:1023-1050` |
| `rt_sigaction`/`rt_sigprocmask` → 0, no state; `sigaltstack` **unhandled → `-ENOSYS`** | `shim.rs:1019-1022`, default arm :1469 |
| `tgkill` fatal-signal path exists; non-fatal signals dropped | `shim.rs:1452-1464` |
| `exit` and `exit_group` conflated (both end the process) | `shim.rs:1465-1468` |
| Zero socket/epoll/eventfd syscalls | grep `SYS_SOCKET|SYS_EPOLL|SYS_EVENTFD` — no hits |
| Clock is virtual (ticks per read; `nanosleep` returns instantly) | `shim.rs:1300-1352` |
| Shim is `Rc<RefCell>`-based, `handle(&mut self, &mut Vcpu, &mut Vm)` — not `Send` | `shim.rs:565-601,691` |
| Process scheduler is cooperative, single-host-thread | `x86jit-linux/src/proc.rs:85-165` |
| **The mt substrate exists and is proven** — clone→host-thread recipe + condvar futex + `Arc<Vm>` run loop, but only inside a *test harness*, not the shim | `x86jit-tests/tests/mt.rs:38-78` (futex), `:152-185` (clone), `:91-121` (run loop); M7 T1–T4b done per the m7-multithreading-tso milestone |
| CPUID reports plain SSE2 x86-64 (no SSSE3/AVX) — Go will select generic code paths | `interp.rs:1436-1482` |
| auxv has `AT_RANDOM`, **no** `AT_SYSINFO_EHDR` → Go uses raw syscalls, no vDSO needed | `x86jit-elf/src/lib.rs:266` |
| `readlink` → `-ENOENT` (no `/proc/self/exe`) | `shim.rs:1263` |
| `getrandom` implemented | `shim.rs:1006` |
| Unknown syscalls degrade to `-ENOSYS` + one-shot gap log — the discovery pipeline | `shim.rs:1469-1482` |

One correction to the session notes: there is **no** "Go hello world runs
single-threaded" milestone. `runtime.main` unconditionally spawns the sysmon M
via `newosproc → clone(CLONE_VM)` before user code, and `newosproc` fatals on
a negative return. The first *printing* Go program lands at the end of
Phase 2, not Phase 1. Phase 1's DoD is therefore "the abort *moves*".

Also load-bearing: Go's `runtime.sigaltstack` asm **crashes the process on any
error** (no errno path). Today `sigaltstack` hits the default `-ENOSYS` arm,
so even with memory + clone fixed, Go dies in `minit`. Phase 3 is not
optional polish; a 2-line stub of it is a Phase 2 dependency.

---

## Phase 0 — Blocking sockets + busybox httpd (the early curl-able win)

**Goal:** a real web page served from the engine, no Go. busybox `httpd` is
fork-per-connection over *blocking* sockets; fork is already proven (OCI-4).

**DoD:** `busybox httpd -f -p 8080 -h /www` under the OCI runner; host
`curl 127.0.0.1:8080/index.html` returns the file. Both engines.

**Work:**
- New `Fd::Socket(OwnedFd)` variant in the shim fd table (`shim.rs` `enum Fd`,
  near :250), bridged to real host fds via `libc`/`std::os::fd` (not
  `std::net` — we need raw `bind`/`accept4`/`setsockopt` control).
- New arms: `socket`(41), `bind`(49), `listen`(50), `accept`(43)/`accept4`(288),
  `connect`(42), `setsockopt`(54) (whitelist `SO_REUSEADDR`, `SO_REUSEPORT`,
  `TCP_NODELAY`, `SO_KEEPALIVE`; unknown → 0), `getsockname`(51),
  `getpeername`(52), `shutdown`(48). sockaddr bytes pass through verbatim
  (guest is Linux x86-64, host is Linux — identical layout; on aarch64 hosts
  sockaddr is also identical, only `epoll_event` differs, see Phase 4).
- Route `read`/`write`/`close` for `Fd::Socket` (extend the existing arms at
  `shim.rs:695,741,760`).
- `LinuxShim::fork()` (`shim.rs:570`) must `dup` socket fds for the child
  (or share via the same `Rc` pattern as pipes) — httpd's forked child
  serves the accepted fd, parent closes it.
- Blocking `accept`/`read` blocks the single scheduler thread. Fine for the
  demo; note it in the arm comment.

**Design decision — bridge vs userspace loopback:** bridge to real host
sockets. The goal *is* a host-visible port; a userspace loopback would need a
second bridge anyway. Trade-off: guest can open real connections (fine for a
local demo runner; add an off-by-default allow flag if it ever matters).

**Effort:** 1–2 sessions. **Risk: low.** The fd-table plumbing is the same
shape as pipes. **Sequencing payoff:** every byte of this (Fd::Socket,
sockaddr handling, accept plumbing) is the substrate Phase 4 extends with
nonblocking mode + epoll. Do it first.

**What could go wrong:** httpd may use `poll` on the listen fd (arm exists at
:1421 but only for pipes — extend to sockets); fork-child fd close ordering.

---

## Phase 1 — Address space: `BigFlat` (host-mmap NORESERVE backing)

**Goal:** Go's `mallocinit` succeeds: ~600 MiB `PROT_NONE` page-summary
reservation + heap-arena reservations (hints start at `0xc000000000` = 768 GiB)
+ lazy commit, with **zero** change to JIT codegen.

**The key design decision.** A true SoftMmu (per-access page-table lookup)
would rewrite the hottest path in both backends — every inlined
`host_base + guest_addr` access (`jit_abi.rs:56`) becomes a lookup, a large
codegen change and a permanent slowdown. **Don't.** Instead keep the flat
one-add translation and make the *backing* sparse at the host level:

- Back the flat model with `mmap(NULL, span, PROT_READ|PROT_WRITE,
  MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE)` with `span` ≈ **1 TiB**
  (`1 << 40`). Host kernel commits zero pages on first touch; untouched
  guest VA costs nothing. 1 TiB covers Go's arena hints (768 GiB) with room.
- Guest `PROT_NONE` reservations become pure bump-arena address grants —
  no touch, no commit, no host cost.

This *is* the spec's SoftMmu slot (§4.1) filled with the cheapest
implementation that satisfies the actual requirement ("sparse, huge, lazily
committed") — record it as an ADR (`backlog/decisions/`) since it forecloses per-page
guest protections (acceptable: `mprotect` is already a no-op, `memory.rs:895`).

**Code sites:**
- `memory.rs:12-18` — either add `MemoryModel::Reserved { span: u64 }` or
  implement the existing `SoftMmu` variant this way (recommend the latter +
  a `span` field; kills the `todo!()` at :307).
- `memory.rs:123` — backing: `vec![]` → mmap wrapper (new small `mod hostmem`
  with mmap/munmap; keep `Vec` for `Flat` so tests/CI stay identical).
- `memory.rs:104-129` **SMC `code_page`** — `Box<[AtomicBool]>` at 1 bool per
  4 KiB page = 256 M entries for 1 TiB. Replace with a two-level table:
  top level `Box<[AtomicPtr<Chunk>]>` (1 GiB granules → 1024 entries), leaf
  chunks allocated on first `mark_code` in that granule. Guest code lives in
  the low image region, so ~1 leaf chunk in practice.
- `memory.rs::deep_copy` (~:141) — cloning 1 TiB is out. Copy only *tagged
  regions* (`self.regions`) intersected with what the shim committed
  (image, `[heap_base, brk)`, `[mmap arena base, mmap_base)`, stack region).
  Simplest correct cut: `deep_copy` walks `regions` and copies those byte
  ranges; the runner already maps exactly the live areas. Fork is only used
  by the busybox/shell path — Go never forks — so a conservative "fork
  requires small Flat" guard is an acceptable interim.
- `shim.rs:843-894` (`SYS_MMAP`) — raise the arena: `mmap_limit` comes from
  the runner layout. Distinguish nothing new; `PROT_NONE` vs RW is already
  irrelevant (no protections). Keep MAP_FIXED-within-span as-is.
- `x86jit-run/src/lib.rs:80-90` — layout: keep `EXE_BASE`/stack as today
  (Go's g0 runs on the initial stack; 8 MiB is plenty), set
  `FLAT_SIZE`→`span = 1 TiB`, `MMAP_LIMIT` → just below span. Gate on a
  runner flag or per-image heuristic if 128 MiB should remain the default
  for the existing corpus.
- `SYS_MADVISE` — add an arm: `MADV_DONTNEED` optionally passes through to
  host `madvise(host_base+addr, …)` so Go's scavenger actually returns RSS;
  everything else → 0. (Currently unknown → `-ENOSYS`; Go tolerates it, but
  the passthrough is ~10 lines and keeps memory honest.)

**DoD (testable):** running `caddy version` moves the fatal error from
`failed to reserve page summary memory` (`mpagealloc_64bit.go:81`) to
`runtime: failed to create new OS thread` — i.e. `mallocinit` and the whole
heap init complete. Plus: existing corpus (busybox three-ways, OCI suite)
green under the new backing; a unit test reserving 600 GiB and touching
3 pages shows RSS < 10 MiB.

**Effort:** 2–4 sessions. **Risk: medium-low.** Contained in `memory.rs` +
layout constants.

**What could go wrong:** 32-bit-ish assumptions anywhere backing `.len()` is
used (audit `memory.rs` fully — e.g. bounds checks, `flat()` helper :631);
aarch64 hosts with 39-bit VA (Raspberry-Pi-class kernels) can't map 1 TiB —
probe and fall back to 256 GiB, which still covers hint #0... it does **not**
cover 768 GiB hints; on such hosts Go's hinted reserve returns a different
address, which Go handles (it accepts arbitrary `sysReserve` results — our
bump arena ignores hints anyway). Verify with the ARM CI runner.

---

## Phase 2 — Threads: `clone(CLONE_VM)` → real host threads (the big one)

**Goal:** Go's M:N runtime boots. This promotes the proven mt-test recipe
(`x86jit-tests/tests/mt.rs`) into the production shim/driver.

**DoD:**
1. The pthreads acceptance program (`x86jit-tests/programs/pthreads.elf`,
   4 threads × mutex counter → deterministic 400000) runs through the
   **shim** (not the test harness), both engines — closes M7-T5 properly.
2. A static Go hello world prints `hello` three ways (native/interp/JIT).
   This is the first Go-runs-at-all demo. (Requires the Phase 3 stub, below.)

**The refactor (why this is the highest-risk phase):** `LinuxShim` is
`Rc<RefCell>`-based and takes `&mut Vm` (`shim.rs:691`); the scheduler
(`proc.rs:85`) is single-threaded cooperative. Threads need N host threads
sharing one `Arc<Vm>` (proven: `tests/threads.rs`, M7-T1) *and* one shared
shim.

**Design decision — shim concurrency model:**
- **(a) `Arc<Mutex<LinuxShim>>`, lock per syscall** — vcpus run lock-free
  (guest compute is the parallel part); syscalls serialize on the process
  lock. Blocking arms (futex WAIT, later epoll_wait/accept/read-socket,
  nanosleep) must **release the lock while blocked** — extract what they
  need, drop the guard, block on a `Condvar`/host call, re-lock to write
  results. Mechanical changes: `Rc`→`Arc`, `RefCell`→`Mutex` in the fd
  table; `handle(&mut self, &mut Vcpu, &Vm)` (Vm loses `&mut` — it's
  already interior-mutable for writes, see `memory.rs` UnsafeCell note).
- (b) A dedicated syscall-server thread + channels: keeps the shim
  single-threaded but forces continuation-style blocking arms.

Pick **(a)** — simpler, and the blocking-arm discipline is needed regardless.

**Work items:**
- New `SYS_CLONE` CLONE_VM branch (`shim.rs:1356`): port the recipe verbatim
  from `mt.rs:152-185` — child `CpuState` = parent clone, `RAX=0`,
  `RSP=stack`, `CLONE_SETTLS`→FsBase, `PARENT_SETTID`/`CHILD_SETTID` writes,
  `CHILD_CLEARTID` recorded; spawn host thread running the vcpu loop over
  `Arc<Vm>` (port `mt.rs:91-121`; on thread exit write 0 to clear_tid + futex
  wake — that's pthread_join/Go's `mdone`). Kernel-exact semantics mean Go's
  `runtime·clone` asm works unmodified (it checks `RAX==0` and jumps to fn;
  it does its own `arch_prctl` settls in the child).
- Real futex (`shim.rs:1023`): port `mt.rs:38-78` (per-address generation +
  `Condvar`), **add FUTEX_WAIT timeout** via `wait_timeout` — Go's timer
  sleeps are `futexsleep(ns)`; without the timeout the scheduler hangs.
- Per-thread identity: `gettid` (`shim.rs:1238` currently == pid),
  `set_tid_address` (:781 pretends tid 1), `tgkill` routing. Thread registry
  in the shim (tid → join handle + clear_tid).
- `exit` vs `exit_group` split (`shim.rs:1465`): `exit` ends *one* thread
  (thread loop returns, clear_tid wake); `exit_group` ends the process
  (flag all vcpus to stop — add a shared stop flag checked via the existing
  run-loop exit path).
- **Clock becomes real** in mt mode: the virtual tick-per-read clock
  (`shim.rs:466,1300-1352`) makes Go's sysmon (`usleep` loop) spin at 100%
  host CPU and confuses netpoll deadlines. Switch `clock_gettime`/
  `nanosleep` to host `CLOCK_MONOTONIC` + real sleep when threads exist;
  keep the virtual clock for the single-threaded deterministic corpus.
  (Deliberate determinism loss — record in `backlog/decisions/`.)
- `sched_yield`(24) → `std::thread::yield_now`, return 0 (Go's `osyield`).
- Driver: `proc.rs` `run_process` (:146) keeps the cooperative *process*
  scheduler; a process now owns a set of vcpu host threads. Rule: **a
  multi-threaded process cannot fork** (return `-EAGAIN`) — Linux fork only
  duplicates the calling thread anyway, busybox never mixes, Go never forks.
- Translation-cache concurrency: already done (M7-T2, `RwLock` cache).
  Atomics: done (M7-T4b — `lock` ops are real atomics in both backends).

**Effort:** 4–8 sessions — the largest item. **Risk: HIGH** (see ranking).

**What could go wrong:**
- Lock-discipline bugs (blocking with the shim lock held → whole-process
  stall). Mitigate: a `debug_assert` helper that blocking arms must use.
- On **ARM hosts**, the weak-ordering half of M7 is *deliberately
  unvalidated* (`m7-multithreading-tso.md` status): run Go under `AcqRel`
  (or `FullTso`) consistency, and treat x86-host-green/ARM-flaky as the
  known bug class, not a mystery. On the x86 dev box this risk is zero.
- Go park/unpark storms exposing futex-generation races — the mt.rs
  implementation is simple; contended-Go is a harsher client than 4
  pthreads. Budget a debugging session.

---

## Phase 3 — Signals: the minimum Go actually needs (no delivery)

**Goal:** Go's `minit`/`initsig` complete honestly; deferred-delivery risk is
made explicit instead of latent.

Go's real requirements at boot, in order:
1. **`sigaltstack` must return 0** — Go's asm wrapper crashes the process on
   any error, and today it's `-ENOSYS` (default arm). *This 2-line stub is a
   Phase 2 dependency* (record per-thread `ss_sp/ss_size`, return old stack
   if `old` ptr non-null).
2. `rt_sigaction` (`shim.rs:1019`): record `sigaction` structs per signal
   (handler, flags, mask, restorer) and **return the old action** when
   queried — Go reads existing handlers during init. Zero-filled old is
   accepted, but recording is ~30 lines and makes later delivery possible.
3. `rt_sigprocmask`: keep returning 0 but **write `oldset`** (zeros) when the
   pointer is non-null.
4. `tgkill(…, SIGURG)` (`shim.rs:1452`): drop it (return 0), as today. This
   disables Go's *async* preemption; cooperative preemption (function-
   prologue checks) still runs, which is how Go worked pre-1.14. For a
   file-serving caddy this suffices — HTTP handling is call-dense.

**Deliberately deferred — real delivery** (frame push on altstack, handler
RIP, `rt_sigreturn` arm restoring the frame, injection at block boundaries
via a per-vcpu pending-signal flag checked in the run loop): only needed for
(a) async preemption of call-free hot loops (symptom: GC stop-the-world
hangs while one goroutine spins), (b) fault→`SIGSEGV`→Go-panic conversion
(symptom: nil-map/nil-deref crashes kill the VM instead of printing a Go
panic). Sketch it in `backlog/docs/deferred.md`; pull forward only on symptom
(a).

> **Update (guard pages, doc-30 / decision-7):** half of (b) is already done. A
> Go nil-deref now **faults visibly** as a resumable `Exit::UnmappedMemory` (host
> `PROT_NONE` guard page → SIGSEGV → `guarded_run`, precise guest RIP), instead of
> silently reading demand-zero — so the fault reaches `report_gap`/`ProcError::Trapped`
> rather than corrupting the run. What remains for (b) is only guest signal
> **delivery**: pushing a Go signal frame and jumping to the runtime's handler to turn
> that `Exit` into a printed Go panic (task-123).

**DoD:** a Go program spawning 100 goroutines doing allocation + channel
ping-pong under GC pressure completes with correct output three ways; the
gap log shows no signal-family `-ENOSYS`.

**Effort:** 1–2 sessions for the ABI-truth layer (+3–5 later iff delivery is
pulled forward). **Risk: low** for the stub layer; the *deferred* half is the
plan's second-biggest unknown (see ranking).

---

## Phase 4 — Networking: nonblocking sockets + epoll (the Go netpoller)

**Goal:** Go's `netpollinit`/`netpoll` work; `net/http` can listen and serve.

Extends Phase 0's `Fd::Socket` with exactly what the Go runtime calls:
- `epoll_create1`(291) → `Fd::Epoll(OwnedFd)` (host epoll fd).
- `eventfd2`(290) → `Fd::Event(OwnedFd)` — Go's `netpollBreak` uses an
  eventfd registered in the epoll set. **Required**, Go crashes if
  `epollcreate`/eventfd init fails.
- `epoll_ctl`(233): translate the *guest fd argument* to the host fd; the
  `epoll_event.data` payload is **opaque** — Go stores its own pointer/fd
  there and the kernel echoes it back, so `epoll_wait` results need **no
  translation at all**. Pass `events` mask through (Go uses
  `EPOLLIN|EPOLLOUT|EPOLLRDHUP|EPOLLET` — edge-triggered passes straight to
  the host epoll, no emulation).
- `epoll_wait`/`epoll_pwait`(232/281): **release the shim lock** (Phase 2
  discipline), block in host `epoll_wait` with the guest's timeout, write
  events back. Another guest thread writing the eventfd wakes it through the
  host kernel — no engine machinery.
- `accept4` with `SOCK_NONBLOCK|SOCK_CLOEXEC`; `fcntl(F_SETFL, O_NONBLOCK)`
  (`shim.rs:1210` arm exists — route socket fds to host fcntl); nonblocking
  `read`/`write` on sockets return `-EAGAIN` from the host verbatim.
- `getsockopt(SO_ERROR)` (Go checks connect completion).
- `sendfile` → already degrades to `-ENOSYS` and Go falls back to a
  read/write loop (the default-arm comment at `shim.rs:1472` even names this
  case). Leave it.

**Design decisions:**
- **Host passthrough, not an emulated readiness layer** — epoll semantics
  (edge-trigger, spurious wakeups, EPOLLRDHUP) are exactly the things you
  don't want to re-implement. The only translation surface is guest-fd →
  host-fd at call time. Trade-off: guest fd numbers and host fd numbers
  diverge (already true for all fds — the table handles it).
- **aarch64 host caveat:** x86-64 `epoll_event` is `packed(4)` (12 bytes);
  aarch64's is 16 bytes/8-aligned. On ARM hosts, repack each record when
  copying to/from guest memory. ~15 lines, gate on `cfg(target_arch)`.

**DoD:** (a) a minimal Go `net/http` hello server under the runner answers
host `curl`; (b) Phase 0's busybox httpd still green (regression guard on the
shared socket plumbing).

**Effort:** 2–4 sessions. **Risk: medium.** The syscalls are mechanical; the
risk concentrates in blocking-without-lock interactions with Phase 2 and in
Go's edge-triggered wakeup expectations (a missed edge = a hung connection —
test with concurrent `curl`s and keep-alive).

---

## Phase 5 — caddy endgame

**Goal & final DoD:** in the `caddy:latest` rootfs under the OCI runner:
`caddy file-server --root /srv --listen :8080` (bypasses the Caddyfile
adapter and TLS on the first pass); host `curl` returns `/srv/index.html`.
Then, stretch: a Caddyfile-configured HTTP site; TLS explicitly out of the
first DoD.

**caddy-specific gotchas beyond generic Go (inspected/known):**
- **Admin endpoint**: caddy binds `127.0.0.1:2019` by default. Passthrough
  handles it; if it becomes noise, `--adapter`-less `file-server` mode or
  `CADDY_ADMIN=off` env in the runner argv.
- **Data/config dirs**: caddy writes `$XDG_DATA_HOME`/`$HOME/.local/share/caddy`
  (autosave, local CA). The shim has host-write resolution
  (`resolve_host_write`, `shim.rs:947`) — ensure the rootfs env gives it a
  writable `HOME`, or set `XDG_*` to a mapped path. File-server mode writes
  little; TLS mode writes a CA — another reason TLS is a stretch goal.
- **`os.Executable`** → `readlink("/proc/self/exe")` → `-ENOENT` today
  (`shim.rs:1263`). caddy calls it for upgrade/plugin paths; error is
  tolerated in `file-server`, but the fix is trivial (return the exec path
  the scheduler already knows) — do it when the gap log shows it.
- **CPUID = plain SSE2** (`interp.rs:1436`): Go crypto/hash pick pure-Go
  fallbacks — *correct but slow*. Fine for HTTP; TLS handshakes will be
  slow under the interpreter (JIT tier-up at 50 hits, `x86jit-run/lib.rs:92`,
  is what makes this viable).
- **Binary size**: caddy ≈ 35–40 MiB of text; translation-cache pressure and
  first-request latency will be visible. Not a correctness risk; note perf
  expectations in the demo.
- **Timezone/DNS files** (`/usr/share/zoneinfo`, `/etc/resolv.conf`): served
  from the rootfs already; Go falls back to UTC/static resolution if absent.
- Expect **2–5 surprise syscalls** in the gap log on first boot
  (`membarrier`, `getrlimit`/`prlimit64`, `epoll_pwait2`, `pipe2` variants
  are the usual suspects). The `-ENOSYS`+log pipeline (`shim.rs:1469`) makes
  each a same-day patch.

**Effort:** 1–3 sessions of gap-chasing and debugging after Phases 1–4 land.

---

## Sequencing, wins, and the honest risk ranking

| Order | Phase | Demoable payoff | Effort | Risk |
|---|---|---|---|---|
| 1 | **P0 sockets+httpd** | **curl-able web page from the engine** (the goal, minus Go) | 1–2 sess | low |
| 2 | **P1 BigFlat** | Go abort *moves* past mallocinit; RSS test | 2–4 | med-low |
| 3 | **P2 threads** (+P3 sigaltstack stub) | pthreads via shim; **Go hello world three ways** | 4–8 | **high** |
| 4 | **P3 signals-minimal** | goroutine/GC stress program green | 1–2 | low |
| 5 | **P4 netpoller** | **Go net/http hello answers curl** | 2–4 | med |
| 6 | **P5 caddy** | **caddy serves index.html — done** | 1–3 | med |

- **Shortcut verdict: yes — do P0 first.** It reaches "a web page served from
  our JIT, curl from the host" *weeks* before Go can, and 100% of its socket
  plumbing is the foundation P4 builds on (accept4/epoll/nonblocking are
  additive). It is shared work, not a detour.
- **Single highest-risk phase: P2 (threads).** It's a structural refactor of
  a 1,768-line `Rc`-based shim into `Send`/lock-disciplined form, it forfeits
  the deterministic virtual clock, and on ARM hosts it sits on the *admittedly
  unvalidated* weak-ordering half of M7. Everything else in this plan is
  additive syscall surface; P2 changes an invariant.
- **Second risk (latent): deferred signal delivery.** If caddy's GC ever
  stalls behind a non-preemptible loop, P3's deferred half (real delivery +
  `rt_sigreturn`) gets pulled in — budget the possibility (+3–5 sessions).
- **Is caddy realistic once the four walls fall? Yes** — caddy is pure-Go/
  cgo-free (it already runs to `mallocinit` as a static binary), and its
  needs beyond generic Go are the enumerable list above (admin port, data
  dirs, `os.Executable`, perf), each small. The fallback ladder if it
  surprises us: Go hello → net/http hello → `caddy file-server` HTTP →
  Caddyfile → TLS. Each rung is a keepable demo.
