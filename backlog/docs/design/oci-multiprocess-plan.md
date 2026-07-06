---
id: doc-19
title: 'OCI-4 multiprocess — implementation brief (fork / pipe / wait / dup2)'
type: specification
created_date: '2026-07-06 11:25'
---

# OCI-4 multiprocess — implementation brief (fork / pipe / wait / dup2)

Goal: run shell entrypoints that spawn **concurrent processes** — `sh -c "echo a |
cat"`, `$(...)` substitutions, forking pipelines. Companion to
[`oci-plan.md`](oci-plan.md) §OCI-4 (D4). This is the largest remaining OCI
subsystem; this brief is the cold-start spec for a fresh session.

> **STATUS: DONE (2026-07-05).** fd-table refactor, `pipe`/`pipe2`, `fork`/`clone`/
> `vfork`, `wait4`, `dup2` through the table, and the deferred-child `Scheduler`
> (`x86jit-linux/src/proc.rs`) all landed. Acceptance met three ways (interp==jit,
> native skipped for rootfs-path reasons): `echo hello | cat`, `echo out-$(echo
> inner)`, `printf 'a\nb\nc\n' | grep b` (`x86jit-run/tests/shell.rs`). 192 tests
> green. Two refinements beyond the original §2 plan, both needed by real busybox:
>
> 1. **Reader-pull for parent-as-reader.** §2's deferred model breaks command
>    substitution, where the *parent* reads a pipe a deferred *child* writes. Fix: a
>    `read` on a would-block pipe (empty buffer, writer still open) yields
>    (`pending_read`); the scheduler runs pending writer children in fork order, then
>    completes the read. On `wait4` the scheduler also reaps *all* pending children in
>    fork order (writer-before-reader) into zombies, so a pipeline's stages run in the
>    right order. Empty pipe + no writers = EOF, so a spurious wake can't loop.
> 2. **`execve` in a child + `close_all_fds` on exit.** The scheduler reloads the
>    image in place on `execve` (fd table + stdout preserved, brk/mmap reset) via a
>    caller-supplied loader — `x86jit-linux` stays ELF-free. Process exit closes all
>    fds so pipe writer/reader counts reach zero (peer sees EOF).
>
> Real D6 gap surfaced and fixed: `write_stat` was forcing mode `0o644`, dropping the
> execute bits, so a shell's PATH search rejected every applet as non-executable.
> Now preserves the real mode. Still deferred (unchanged): true concurrency and
> full-pipe backpressure; `O_CLOEXEC` (fds all survive `execve` for now).

House rules unchanged: interp == JIT == Unicorn on any new instruction; core stays
guest-agnostic (the `core_stays_guest_agnostic` tripwire); every real image adds
≤1 thing via the D6 gap pipeline; commit only on explicit request; work on `main`.

## 0. What already exists (do NOT rebuild)

- **Core fork primitive — DONE** (commit `8c4dc94`): `Vm::fork_with_backend(&self,
  Box<dyn Backend>) -> Vm` (`x86jit-core/src/vm.rs`) + `Memory::deep_copy`
  (`x86jit-core/src/memory.rs`). Deep-copies guest memory + region tags, fresh
  cache, child gets a caller-supplied backend, inherits consistency + tier-up. Test:
  `fork_gives_the_child_independent_memory`. This is the ONLY core change needed —
  everything else is embedder-side.
- **execve — DONE** (commit `905d966`): single-command shell already works.
  `x86jit-linux` `SYS_EXECVE` parses `(path, argv[], envp[])` into
  `LinuxShim.pending_exec: Option<ExecRequest>` (+ `read_cstr_array` helper);
  `x86jit-run::run_config_argv` loops over process images, reloading on
  `pending_exec` via the extracted `load_process` helper. Reuse this
  request-back-to-driver pattern for fork/wait.
- **Graceful `-ENOSYS`** (commit `507d7b3`): unknown syscalls log once
  (`gap:syscall`) + return `ENOSYS`; `LinuxShim.gap_syscalls: HashSet<u64>`.
- Runner: 3 ELF shapes (dynamic PIE / static-PIE / ET_EXEC), `serve_rootfs`
  GuestFs, tiering on. All in `x86jit-run/src/lib.rs`.

## 1. Verified gap order

Probed `busybox sh -c "echo hello | cat"` (busybox:musl fixture): the first gap is
**`pipe` (syscall 22)** — the shell can't even create the pipe, so it never reaches
fork. Order to implement: **pipe(22) → clone/fork → wait4(61) → dup2(33 — a stub
exists, verify it routes through the new fd table)**. Harmless probes seen:
getppid(110), uname(63) — already `-ENOSYS`, fine.

x86-64 numbers: `pipe=22 pipe2=293 clone=56 fork=57 vfork=58 wait4=61 dup=32
dup2=33 dup3=292 execve=59 exit_group=231`.

## 2. Design: deferred-child, sequential, buffered pipes

Chosen over thread-per-process (mt.rs style). Rationale: deterministic (matters for
the differential oracle — thread scheduling is nondeterministic), no shared-Vm
concurrency, and it handles the common shell pipeline. Trade-off (document it): a
process that FILLS a pipe and blocks *before* its reader runs would deadlock — avoid
by giving pipe buffers **unbounded** capacity (no writer ever blocks). Real shell
one-liners move tiny data, so this is fine. Truly-concurrent servers and full-pipe
backpressure are out of scope for this rung.

Mechanics:
- A guest **process** = a `Vm` + its `CpuState` + an fd table + brk/mmap cursors.
- `fork` (via `Vm::fork_with_backend`): snapshot the parent, clone the fd table
  (pipe ends share their `Arc` buffer — fd inheritance), child `CpuState` = parent's
  with `RAX = 0`, record the child as **pending** (do NOT run it yet), return the
  child pid to the parent, which keeps running.
- `wait4(pid)`: run the pending child to completion NOW (it reads/writes the
  buffered pipes), collect its exit code, return it to the parent.
- **stdout**: each process has its own capture; the driver **concatenates** child
  stdouts in execution order (deferred children run sequentially, so order is
  well-defined). No shared stdout needed. `echo`→pipe writes nothing to stdout;
  `cat` writes "hello\n"; concatenation = "hello\n".
- **pipes** DO need a shared buffer (`Arc<Mutex<VecDeque<u8>>>` + reader/writer open
  counts): `echo` writes it, `cat` reads the same buffer; the buffer survives fork
  via fd-table cloning.

## 3. The fd-table refactor (the crux — do this first, carefully)

Today `SYS_WRITE`/`SYS_READ` special-case fd 1/2 (stdout/stderr) and route fd ≥ 3 to
`FsPassthrough.open_files: HashMap<u64, OpenEntry>` where `OpenEntry = File | Dir`
(`x86jit-linux/src/shim.rs`). Pipes + `dup2` require **every** fd routed through one
table, because `dup2(pipe_write, 1)` makes `write(1)` go to a pipe, not stdout.

Introduce a unified fd model:

```rust
enum Fd {
    Stdout,                              // -> the process's stdout capture
    Stderr,                              // -> stderr capture
    Stdin,                               // -> the shim's stdin buffer
    File(Rc<RefCell<OpenEntry>>),        // host file/dir (Rc so dup2 aliases share offset)
    PipeRead(Rc<RefCell<PipeBuf>>),
    PipeWrite(Rc<RefCell<PipeBuf>>),
}
struct PipeBuf { data: VecDeque<u8>, writers: usize, readers: usize }
// fd table: BTreeMap<u64, Fd> seeded with 0->Stdin, 1->Stdout, 2->Stderr.
```

Use `Rc<RefCell<>>` (single-threaded deferred model — no threads, so no `Arc/Mutex`
needed; if you later go concurrent, swap to `Arc<Mutex<>>`). `File` moves from a
bare `File` into `Rc<RefCell<OpenEntry>>` so `dup`/`dup2` alias share the seek
offset (POSIX).

Refactor, keeping the differential suite green at each step:
1. Replace `open_files` + the 1/2 special-cases with the `fd_table: BTreeMap<u64,
   Fd>`. `do_open` inserts a `File` at the next free fd. `SYS_WRITE`/`do_read` match
   on `fd_table.get(&fd)` uniformly (`Stdout`→push to `self.stdout`, `File`→host
   read/write, `PipeWrite`→append to buf, `PipeRead`→read from buf).
2. `SYS_CLOSE` removes from `fd_table`, decrementing pipe reader/writer counts (EOF
   when writers hit 0; EPIPE when readers hit 0 — but with unbounded buffers, a
   reader just gets 0 bytes at EOF).
3. `SYS_DUP`/`SYS_DUP2`/`SYS_DUP3`: clone the `Fd` (an `Rc` clone shares the
   underlying file/pipe) at the target fd.
4. `next_fd` allocation: lowest free fd ≥ 3 (dup2 can target any fd).

**Gate:** after this refactor, run the whole suite (185 tests) — sqlite_file,
python, gzip all exercise real fds. Zero behavior change is the bar before adding
pipes.

## 4. Syscalls to add

- **`pipe`(22) / `pipe2`(293)**: allocate a `PipeBuf` (`Rc<RefCell>`), insert a
  `PipeRead` and a `PipeWrite` at two fresh fds, write the two fd numbers to the
  guest `int[2]` at the pointer in RDI (`vm.write_bytes`). `pipe2`'s flags (RSI):
  honor `O_CLOEXEC` (mark the fd cloexec — matters for execve, below); ignore the
  rest.
- **`clone`(56) / `fork`(57) / `vfork`(58)**: busybox musl likely uses `clone` with
  `SIGCHLD` (process) or `vfork`. Detect: if `clone` flags (RDI) include `CLONE_VM`
  (0x100) it's a *thread* — that path is mt.rs's job and out of scope here; return
  `-ENOSYS` for CLONE_VM for now (log it). Otherwise it's a process fork: build a
  `ForkRequest` (like `ExecRequest`) — the driver does the actual `Vm::fork` because
  it owns the Vm. The shim signals `pending_fork = Some(ForkRequest)` and returns
  true; the driver clones the Vm + fd table, records the child pending, sets parent
  RAX = child_pid, and re-enters the parent's run loop. **Subtlety:** unlike execve
  (which leaves `run()` for good), fork must RESUME the parent after — so the driver
  needs to re-enter the same parent Vm at the instruction after the syscall (RIP is
  already past `syscall`). Structure the driver so a fork request suspends the
  current process, not ends it.
- **`wait4`(61) / `waitpid`**: pending-child bookkeeping. `wait4(-1 or pid, status*,
  ...)`: pick a pending/zombie child; if pending, run it to completion (a nested
  driver loop on the child Vm+shim-state); write its exit status to the `status*`
  pointer (encode as `(code & 0xff) << 8` for normal exit); return the child pid.
  With no children left, return `-ECHILD`.
- **`dup2`/`dup3`**: see §3.3. A `SYS_DUP2` stub may already exist — make it route
  through the fd table.
- **`execve` interaction**: on execve, close cloexec fds; keep the rest (pipes stay
  open across exec — that's how `cat` inherits its stdin pipe). The current execve
  resets the whole process image; make it preserve the fd table (minus cloexec).

## 5. Driver / scheduler (`x86jit-run` + maybe `x86jit-linux::proc`)

`run_config_argv` currently loops over execve images with one shim. Generalize to a
process scheduler:

- A `Process { vm, cpu: Vcpu, shim_state }`. The fd table + stdout + brk live in the
  per-process shim state; pipes are shared `Rc`s across processes.
- The driver runs the **root** process until it exits or blocks. On a `ForkRequest`,
  clone into a pending child (deferred). On `wait4`, run the referenced pending
  child to completion (recursively — a child may itself fork), then resume the
  parent. On execve, reload the image in place (already done).
- **stdout assembly:** collect each process's stdout as it finishes; the final
  `RunResult.stdout` is the concatenation in completion order. For `echo|cat`: sh
  forks echo (pending), forks cat (pending), waits both → run echo (stdout empty,
  writes pipe), run cat (reads pipe, stdout "hello\n") → concat = "hello\n".
- Decide where this lives: a new `x86jit-linux::proc` module (promotable, reusable)
  or in `x86jit-run`. `x86jit-linux::proc` is cleaner (the OS process model belongs
  in the embedder), but it needs the ELF loader — `x86jit-linux` would gain an
  `x86jit-elf` dep. Acceptable (elf is an embedder-side crate). The `mt.rs`
  clone/futex thread machinery (`x86jit-tests/tests/mt.rs`) should also graduate
  here eventually so threads + processes share a substrate (plan D4), but that can
  be a follow-up — threads already work in mt.rs.

## 6. Acceptance + invariants

- New `x86jit-run/tests/pipe.rs`: `sh -c "echo hello | cat"` interp == JIT ==
  "hello\n" (native leg skipped — same rootfs-path reason as the execve test). Then
  `sh -c "echo $(echo nested)"` (command substitution = fork + pipe + wait). Then a
  two-stage pipeline `printf 'a\nb\nc\n' | grep b`.
- The whole existing suite (185) must stay green — the fd-table refactor is the risk;
  gate on it before pipes.
- If a real distro shell surfaces an instruction gap, run the D6 pipeline (lift +
  interp + JIT + differential + compat map refresh).
- Keep the deferred-model limitation documented (full-pipe backpressure / true
  concurrency deferred).

## 7. Concrete first task (start here)

**The fd-table refactor, behavior-preserving, no new syscalls.** Replace
`FsPassthrough.open_files` + the fd-1/2 special cases in `SYS_WRITE`/`do_read`/
`SYS_CLOSE`/`SYS_DUP*` with the unified `fd_table: BTreeMap<u64, Fd>` (§3), seeded
0/1/2 → Stdin/Stdout/Stderr, files as `Rc<RefCell<OpenEntry>>`. No pipes yet. Land
it when `cargo nextest run --workspace --all-features` is fully green (185) — proving
the refactor is a pure internal restructuring. That green baseline is the platform
everything else (pipe/fork/wait) builds on.

## Files that change

- `x86jit-linux/src/shim.rs` — the fd-table refactor + pipe/fork/wait/dup2 arms +
  `pending_fork`/wait bookkeeping. (Biggest change.)
- `x86jit-run/src/lib.rs` — the process scheduler (or a new `x86jit-linux/src/proc.rs`).
- `x86jit-run/tests/pipe.rs` — acceptance.
- `x86jit-core/*` — nothing (the fork primitive is done; resist adding more to core).
