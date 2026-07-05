# oci — phased implementation plan

Companion brief: [`oci-brief.md`](oci-brief.md). House style follows
[`fast-dispatch-plan.md`](fast-dispatch-plan.md) / [`superblock-plan.md`](superblock-plan.md).
Authored by the Fable 5 planning agent; implemented by Opus. Sole decider: Mikołaj.

Goal: **read and run OCI/Docker container images on the x86jit recompiler**,
offline, climbing the real Linux userland surface. If an image needs newer
instructions we *add the instructions*, never dumb down the image.

## 0. Ground truth from the code (verified — with corrections to the brief)

1. **Workspace**: 5 crates (`x86jit-core`, `x86jit-cranelift`, `x86jit-elf`,
   `x86jit-tests`, `x86jit-bench`). Deps clean: core → `iced-x86` only; cranelift/elf
   → core; tests → all; **`x86jit-bench` → `x86jit-tests`** for `LinuxShim`
   (`x86jit-bench/src/workloads.rs`) — the shim graduation must repoint bench too.
2. **Shim ground truth — brief off by ~2×**: `x86jit-tests/src/syscall.rs` (1023
   lines) defines **57** syscall constants, not ~100. `futex` has a shim arm, but
   **`clone` is NOT in `LinuxShim`** — thread spawning lives in the *test harness*
   `x86jit-tests/tests/mt.rs` (`handle()`, clone, real blocking futex in the test's
   `Shared`). Graduating the shim also means graduating mt.rs's thread-spawn/futex
   machinery. Missing today: `uname`, `statx`, `sysinfo`, `madvise`, `mremap`,
   `pipe`/`pipe2`, `fork`/`vfork`/`execve`/`wait4`, `kill`/`tgkill`, `sigaltstack`,
   `socket`/*, `poll`/`epoll_*`, `eventfd`, `nanosleep`.
3. **CPUID is already inconsistent — a live trap.** `cpuid_run`
   (`x86jit-core/src/interp.rs:1249`) advertises the full **v2 line** in leaf-1 ECX —
   SSE3, SSSE3, CMPXCHG16B (bit 13), SSE4.1, SSE4.2, POPCNT — plus MMX/FXSR in EDX.
   Its own doc comment contradicts the code, and the lifter implements **none of**:
   `cmpxchg16b` (advertised via bit 13!), `palignr`, `movddup`, `lddqu`,
   `movshdup/movsldup`, `pabs*`, `phadd*`, `pmaddubsw`, `pmulhrsw`,
   `pcmpistri/pcmpestri`, `ptest`, `round{ss,sd,ps,pd}`, `pmovzx*/pmovsx*`, `pmulld`,
   `blendv*`, MMX entirely. glibc IFUNC resolvers probe exactly these bits then jump
   into those instructions; `glibc.rs` passes by fixture luck. **Verified:** the ecx
   bits (SSE3|SSSE3|CX16|SSE4.1|SSE4.2|POPCNT) are set; lift has 0 arms for
   cmpxchg16b/palignr/ptest/movddup/pmovzx/pmulld. Only a mechanical map can be
   trusted — hand prose is already wrong in-tree.
4. **Even v1 baseline has holes.** Absent from `lift_insn`: `shld/shrd`, `rcl/rcr`,
   `lahf/sahf`, `cmpxchg8b`, `movmskps/movmskpd`, `movnt*`, `maskmovdqu`,
   `pmullw/pmulhw/pmulhuw/pmuludq`, `pmaddwd`, `psadbw`, `pavgb/pavgw`,
   `packsswb/packssdw` (only `packuswb` exists), all packed int↔float conversions,
   `ud2`/`int3` arms. x87 is a fixed subset (f64-backed, no 80-bit, no
   `fsqrt/fsin/frndint/fisttp`). **No VEX/AVX anywhere.**
5. **Unsupported-instruction plumbing exists (the forcing-function hook)**:
   `LiftError::Unsupported { addr, bytes: [u8;15], len }` → `Exit::UnknownInstruction`.
   The runner re-decodes those bytes with iced and classifies by
   `Instruction::cpuid_features()`.
6. **Three-way harness** (preserve verbatim): `x86jit-elf` load
   (`load_static_elf`/`load_dynamic_elf`/`setup_stack`), loop on
   `cpu.run(&vm, None)`, service `Exit::Syscall` via `LinuxShim::handle`, compare vs
   `reference()`/`reference_dyn()` native subprocess — which already **skips the
   native leg on non-x86 hosts** (the precedent for images whose entrypoint can't run
   natively). See `whole_program.rs::run_program`, `dynamic.rs`, `glibc.rs`.
7. **Tiering shipped**: `Vm::set_tier_up_after` — the image runner is its client.
8. **CI is manual-dispatch only**; the real gate is local `cargo nextest run`. "CI
   enforcement" for the compat map = an ordinary default-suite `#[test]`.
9. **Fixture precedent**: multi-MB binaries already checked in (python3.elf,
   busybox.elf, ld-linux, sqlite, lua). Vendoring small image tars is consistent.

## 1. Design decisions

### D1. Crate architecture: two new library crates + one binary; core untouched except instruction semantics

```
                       ┌───────────────────────────────────────────────┐
                       │                x86jit-run (bin)                │  CLI: run / scan / report
                       └───────┬───────────────┬───────────────┬───────┘
                               │               │               │
                    ┌──────────▼─────┐  ┌──────▼───────┐        │
                    │  x86jit-oci    │  │ x86jit-linux │        │
                    │ image tar/JSON │  │ syscall shim │        │
                    │ layers→rootfs  │  │ GuestFs      │        │
                    │ config extract │  │ threads      │        │
                    │ (NO core dep)  │  │ processes    │        │
                    └────────────────┘  │ signals/pipes│        │
                                        └──┬────────┬──┘        │
                                           │        │           │
                                    ┌──────▼───┐ ┌──▼───────────▼──┐
                                    │x86jit-elf│ │ x86jit-cranelift │
                                    └──────┬───┘ └──────┬───────────┘
                                           │            │
                                        ┌──▼────────────▼──┐
                                        │   x86jit-core    │   guest-agnostic; deps: iced-x86 only
                                        └──────────────────┘
   x86jit-tests ──► core, elf, cranelift, x86jit-linux (dev)   (harness unchanged)
   x86jit-bench ──► repointed from x86jit-tests to x86jit-linux for the shim
```

- **`x86jit-linux`** (new): the Linux x86-64 userland embedder. Owns the syscall shim
  (moved **verbatim** from `x86jit-tests/src/syscall.rs` via `git mv`, module
  `x86jit_linux::shim`, `LinuxShim` keeps its exact public API); later the `GuestFs`
  rootfs resolver (D3), the thread spawner promoted from `mt.rs`, the multi-process
  `Kernel` (D4). Deps: `x86jit-core`, `x86jit-elf`.
- **`x86jit-oci`** (new): pure image-format crate. Parses `docker save` tars (legacy
  `manifest.json` + OCI `index.json`/`blobs/sha256` — docker ≥25 emits the latter),
  applies layers with whiteout semantics (`.wh.<name>`, opaque `.wh..wh..opq`),
  extracts to a rootfs dir (traversal-safe against `..`/absolute paths — untrusted
  tars; `tar` crate `unpack_in`), returns `ImageConfig { env, cmd, entrypoint,
  working_dir, user }`. Deps: `tar`, `flate2`, `serde_json`. **No `x86jit-core`
  dependency at all** — the strongest boundary: cannot leak into the recompiler.
- **`x86jit-run`** (new bin, `publish = false`): thin composition — oci (rootfs+config)
  → linux (process/shim/fs) → elf (load) → core/cranelift (execute, tiering on).
  Subcommands: `run <image.tar>` (`--backend interp|jit|both`, `--cmd`, `--env`,
  `--gap-report <json>`), `scan <image.tar>` (static ISA pre-scan, D2), later `pull`
  (deferred; offline-first).
- **What may touch `x86jit-core`**: (a) new guest **instruction semantics**
  (lift/interp/IR/x87) — the point; (b) at most one small **guest-agnostic**
  addition for OCI-4: a memory snapshot/clone entry point on `Vm`/`Memory` (for
  `fork`; a §4.2 memory-level facility like `write_bytes`, knows nothing about
  processes). Nothing else: no tar, JSON, paths, syscall numbers, pids.
- **Graduation without breaking tests**: one commit `git mv` the shim + mechanical
  import rewrites across ~12 integration tests + `x86jit-bench/src/workloads.rs`;
  `x86jit-tests` gains `x86jit-linux` dep, drops the module. Zero behavior change;
  full suite green before anything else lands.
- **Boundary tripwire**: default-suite test `core_stays_guest_agnostic` reads
  `x86jit-core/Cargo.toml` and asserts `[dependencies]` == `{iced-x86}`. Turns the
  sacred rule into a red test.

### D2. The compatibility-map system (headline deliverable)

**Principle: the map is computed by probing the actual lifter, never hand-written.**

- **Mechanism** — new `x86jit-tests/src/compat.rs` + bin `src/bin/compat.rs` (needs
  core `lift_block` + iced encoder + serde — all deps; no Unicorn, probing lifts but
  never executes):
  1. Enumerate every `iced_x86::Code` (`Code::values()`).
  2. Scope filter: keep codes whose `cpuid_features()` intersect the generation map,
     64-bit-decodable; a checked-in denylist (`x86jit-tests/compat/out-of-scope.ron`)
     excludes system/privileged/legacy codes with a one-line reason each.
  3. For each code, synthesize a canonical `Instruction` (templated operands), encode
     with iced's `Encoder`, place bytes in a scratch `Memory`, call `lift_block`.
     Classify **Lifted** / **Unsupported** / **Unencodable**. Probe **both** reg and
     mem operand shapes where the form allows — catches "register source only for
     now" partial coverage.
  4. Generation map (checked-in constant): **v1** = base+SSE+SSE2(+CMOV, FXSR); **v2**
     = SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT, CMPXCHG16B, LAHF-SAHF; **v3** = AVX, AVX2,
     BMI1, BMI2, FMA, F16C, LZCNT, MOVBE, XSAVE; **v4** = AVX-512 F/BW/CD/DQ/VL. Plus
     non-level groups: x87 (with a **fidelity** column — implemented-as-f64), MMX,
     string/atomics.
- **Artifacts** (generated, checked in): `wiki/compat/coverage.json` (per-feature
  total/lifted/partial/missing code lists) + `wiki/compat/isa-coverage.md` (per-gen
  table with percentages — the dashboard).
- **Enforcement — two default-suite tests**:
  - `compat_map_is_current`: regenerate in-memory, diff vs checked-in files; mismatch
    fails with "run `cargo run -p x86jit-tests --bin compat -- --write`". Adding a
    lift arm without refreshing the table = red test.
  - `cpuid_advertises_only_what_lifts`: decode the bits `cpuid_run` returns (leaves
    1/7/0x80000001) and assert every advertised feature's in-scope codes probe
    **Lifted**, minus a checked-in waiver file `compat/cpuid-waivers.ron` (each waiver
    carries a gap-issue number + rationale). **Fails on today's tree by design**
    (CMPXCHG16B, SSE3/SSSE3/SSE4.x, MMX).
- **Gating images**: `x86jit-run scan <image>` decodes the entrypoint ELF + rootfs
  `.so`s' executable sections, buckets `Code`s by generation, prints "image needs: v2
  (12 codes), v3 (3: vpbroadcastb…) — engine: v1 98%, v2 61%, v3 0%". At runtime, on
  `Exit::UnknownInstruction` the runner re-decodes `bytes`, prints
  mnemonic+`cpuid_features()`+generation+map row, appends to `--gap-report` JSON.
- **CPUID stays hand-written but machine-checked** (deriving it from the map is
  over-engineering; the consistency test makes drift impossible). To advertise a
  feature, first make the map show 100% for it.

### D3. Rootfs serving: `GuestFs` in `x86jit-linux` (evolution of `FsPassthrough`)

- `GuestFs { root: PathBuf, cwd: GuestPath }`: normalize guest path (`.`/`..`
  lexically within the guest namespace), walk components resolving symlinks **against
  the rootfs root** (a symlink to `/etc/passwd` → `rootfs/etc/passwd`, never host's),
  cap symlink depth. Writes go in-place into the extracted rootfs (per-run temp dir —
  cheap, correct, no overlay in the MVP).
- Installed as an alternative resolver inside `LinuxShim`
  (`enum PathPolicy { Allowlist(FsPassthrough), Rootfs(GuestFs) }`); existing
  allowlist mode + its ~12 tests untouched.
- Synthetic minimum: `/dev/null`, `/dev/zero`, `/dev/urandom` (deterministic PRNG
  option), `/proc/self/exe` readlink.

### D4. Multi-process model (the OCI-4 subsystem)

- **One guest process = one `Vm` + its vcpus + one `ProcState`**, main vcpu on a
  dedicated host thread (the proven `mt.rs::run_vcpu` shape, promoted into
  `x86jit-linux::proc`). Separate `Vm`s = separate address spaces + caches for free;
  **no shared page cache in this track** (content-hash-keyed shared cache = future
  work aligned with FD-AOT prereqs — same "cache key = guest-byte hash").
- **`Kernel`** (`Arc`-shared): pid allocator, process table (parent links, exit
  status, wait condvars), the **open-file-description table** — `FdTable:
  Vec<Option<FdEntry { ofd: Arc<Mutex<OpenFileDescription>>, cloexec: bool }>>` so
  `fork` shares seek offsets through the `Arc` and `execve` applies `O_CLOEXEC`; a
  refactor of the shim's per-shim `open_files`.
- **`fork`**: snapshot parent `Vm` memory into a fresh `Vm` (the one core addition —
  `Vm::snapshot_memory()`, guest-agnostic §4.2), clone the calling vcpu's `CpuState`
  (child RAX=0), deep-copy `ProcState` (fd entries cloned, `Arc<OFD>` shared), spawn a
  host thread. Fork from a multithreaded process copies only the calling thread.
  Child cache starts cold (tiering re-warms; COW later).
- **`vfork`** = fork + parent blocked until child `execve`/`exit`. Makes `posix_spawn`
  free (userspace vfork+exec).
- **`execve`**: resolve via `GuestFs`, handle `#!` shebang, build a fresh `Vm`
  (static/dynamic via elf, `PT_INTERP` from rootfs), swap into the process slot; pid,
  ppid, fd table (minus cloexec) persist.
- **`pipe`/`pipe2`**: `PipeBuf { VecDeque<u8>, readers, writers, Condvar }` as an OFD
  variant; EOF/EPIPE from counters; condvar blocking (each process is a host thread).
- **`wait4`/`waitpid`**: block on child's exit condvar; reap zombies. **Signals,
  minimal but honest**: per-process pending mask + `sigaction` table, delivery at
  syscall boundaries and on `Exit::BudgetExhausted` (run children with a block budget
  so `kill` interrupts within bounded time); SIGCHLD on child exit. Full guest
  signal-frame delivery (`rt_sigreturn`, trampolines) is its own task (Go/supervisors
  need it).
- **Three-way**: native leg where host-runnable; else interp==JIT (the `reference()`
  skip pattern).

### D5. Three-way comparison preserved at every rung

`x86jit-run --backend both`; every acceptance test runs interp+JIT and diffs; native
leg runs whenever the entrypoint is host-executable (static directly; dynamic
best-effort via the rootfs's own `ld-*.so --library-path`), else skipped. New
instructions always get full interp==JIT==Unicorn independent of images (D6).

### D6. The instruction-adding pipeline (repeatable, no drift)

1. **Surface**: `x86jit-run` hits `Exit::UnknownInstruction` → prints classification
   (mnemonic, CpuidFeature, generation, compat row) → `--gap-report` JSON.
2. **Log**: one GH issue per instruction *family* (`gap:insn` + `track:oci`, title
   `lift: <mnemonics> (<feature>)`), body from the runner's report. Syscalls likewise
   `gap:syscall`.
3. **Implement** in fixed order: IR op if needed (`ir.rs`) → lift (`lift.rs`) →
   interp (`interp.rs`) → JIT (`codegen.rs`; shared helper via `interp` where subtle).
4. **Test**: capture-CLI RON vectors for edge cases (every bug = a vector *before* the
   fix), add mnemonic to the fuzz pool (`fuzz.rs`), run interp==JIT==Unicorn.
5. **Refresh the map**: `cargo run -p x86jit-tests --bin compat -- --write`;
   `compat_map_is_current` forces it in the same PR.
6. **Re-run the image** that surfaced it; close the issue with the image name.

One family per PR, `feat(lift): <family>`. **Oracle caveat**: Unicorn 2.1 (QEMU-5)
predates TCG AVX (QEMU 7.2) — at v3/AVX the differential oracle becomes the *native
x86-64 host* leg, Unicorn covers ≤ v2. Flag in the v3 task.

### D7. Branch strategy and FD-AOT sequencing

- **Run on `main` via short-lived per-rung branches** (`feat/oci-0-compat-map`, …),
  not one long-lived track branch: every rung is independently landable, new crates
  can't destabilize existing ones (additive dep edges only), and the compat-map tests
  gate all concurrent work on `main` immediately. (Differs from fast-dispatch's single
  branch deliberately — that mutated one hot file; this mostly adds crates.)
- **FD-AOT**: stays deferred behind OCI-1…OCI-3. OCI is embedder-side, touches none of
  the AOT prereqs (slot/helper-table indirection, `is_pic`, relocations), so tracks
  are file-disjoint and interleave; but sequence AOT *after* OCI-3, because the image
  runner supplies its missing inputs: compile-heavy one-shot workloads in
  `x86jit-bench` whose cold-start numbers justify/kill AOT, and the content-hash cache
  key. Interim mitigation shipped: runner defaults to `set_tier_up_after(Some(~50))`.

## 2. Phases

Each independently landable; whole suite green (differential/fuzz/corpus vs Unicorn;
whole-program; smc/threads/mt/tso; jit/superblock/cache) + new compat tests; clippy
clean.

### OCI-0 — Compatibility map + CPUID consistency (the gating tool; the first task)

- T1: probe harness (`compat.rs` + `bin/compat.rs`), generation map, out-of-scope
  denylist, generated `coverage.json` + `isa-coverage.md`, `compat_map_is_current`.
- T2: `cpuid_advertises_only_what_lifts` — **red on the current tree**. Recommended
  resolution: keep v2 advertisement as the *target*, add expiring waivers tied to
  `gap:insn` issues for SSE3/SSSE3/SSE4.x (scheduled in OCI-3), but **un-advertise
  CMPXCHG16B and MMX immediately** (advertised-and-plausibly-used-but-not-lifted is
  the live trap). Fix the stale `cpuid_run` doc comment.
- T3: `core_stays_guest_agnostic` manifest tripwire.
- No image code. Engine work: none. Deliverable: the dashboard exists and cannot rot.

### OCI-1 — Shim graduation + image loader + runner MVP (acceptance: `hello-world`)

- T1: create `x86jit-linux`; `git mv` shim verbatim; rewrite imports in ~12 tests +
  bench; suite green (pure move).
- T2: create `x86jit-oci`: `docker save` tar (legacy + OCI layout), layer application
  with whiteouts, traversal-safe extraction, `ImageConfig`. Unit-tested vs tiny
  checked-in fixture tars incl. a whiteout/opaque-dir case (built by a committed
  script, vendored — offline).
- T3: `GuestFs` behind `PathPolicy`; existing allowlist tests untouched.
- T4: `x86jit-run`: `run` (single static entrypoint, no fork; env/argv/cwd from
  config; tiering on; `--backend both`; UnknownInstruction classifier + gap-report)
  and `scan`. Vendor the official `hello-world` tar (~15 KB).
- Acceptance `x86jit-tests/tests/oci.rs::hello_world_image_runs_three_ways`: extract →
  native/interp/JIT → identical stdout + exit code.

### OCI-2 — Static single-process climb (acceptance: `busybox` + a static-musl Rust image)

- Images: official `busybox` (vendored) running applets directly (`echo`,
  `sha256sum`, `cat` — no `sh -c`); a `FROM scratch` static-musl Rust hello.
- Work: syscalls `uname`, `sysinfo`, `statx`, `clock_getres`, `madvise`(no-op),
  `mremap`, `nanosleep`, `fadvise64`(no-op); graceful `-ENOSYS` default with a
  logged-once warning instead of panic (unknown-syscall path becomes a gap reporter
  symmetric to `UnknownInstruction`). **Static-PIE** in `x86jit-elf` (`ET_DYN` without
  `PT_INTERP` — distroless/Go).
- Instruction gaps likely: `cmpxchg16b` (then re-advertise bit 13), maybe
  `psadbw`/`pavg*` — via D6.
- Stretch (optional, own task): a static Go image → `sigaltstack`, `tgkill`, signal
  frames, `epoll_create1`; may move to OCI-4.

### OCI-3 — Dynamic glibc/musl at scale (acceptance: `alpine` vendored; `debian:stable-slim` digest-pinned)

- Embedder: loader generalization — read `PT_INTERP` from the image binary, open
  `ld-linux`/`ld-musl` from the rootfs via `GuestFs`, choose bases; library search
  through the rootfs replaces `serve_lib` suffix hacks.
- **Big engine sub-track — v1/v2 completion, driven by the map**: SSE2 completion
  first (`pmuludq/pmullw/pmulhw/pmulhuw`, `pmaddwd`, `psadbw`, `pavg*`,
  `packsswb/packssdw`, `movmskps/movmskpd`, packed cvt family, `movnt*` as plain
  stores, `shld/shrd`, `lahf/sahf`, `cmpxchg8b/16b`), then waivered v2 families
  (SSSE3: `palignr`, `pabs*`, `phadd*`, `pmaddubsw`, `pmulhrsw`, `psign*`; SSE3:
  `movddup`, `lddqu`, `movshdup/movsldup`, `addsubp*`, `haddp*`; SSE4.1:
  `pmovzx/pmovsx`, `pmulld`, `ptest`, `round*`, `pinsr*`, `pmin/pmax` variants,
  `blendv*`, `packusdw`; SSE4.2 `pcmpistri/pcmpestri` only if glibc hits them). Each
  family: one issue, one PR, full D6; waivers retire as families land; the v2 row
  marches to 100%, then CPUID v2 advertisement is clean.
- Syscalls likely: `sched_getaffinity`, `getrusage`, `umask`, `faccessat2`, more
  `fcntl`, `/proc` reads.
- Acceptance: alpine `/bin/busybox cat /etc/os-release` + debian `/usr/bin/md5sum
  /etc/os-release` three-way.

### OCI-4 — Multi-process (acceptance: an image whose ENTRYPOINT is a shell script)

- Build the D4 subsystem in `x86jit-linux::{kernel, proc}`: `Kernel`/process table,
  `FdTable` with `Arc<OFD>`, `fork` (+ the one core addition: `Vm` memory snapshot —
  reviewed vs §1/§4.2), `vfork`, `execve` (+shebang), `wait4`+zombies, `pipe/pipe2`,
  `dup2` across the OFD table, `getppid`, `setpgid`/`getpgrp` stubs, signal minimum
  (pending mask, SIGCHLD, `kill`/`tgkill`, delivery at syscall/budget boundaries),
  then guest signal-frame delivery (`rt_sigreturn`) as its own task.
- Promote `mt.rs`'s clone/futex thread machinery into `x86jit-linux` so threads +
  processes share the spawning substrate.
- Acceptance: alpine/busybox, `ENTRYPOINT ["/bin/sh","-c","echo start; ls /etc | wc
  -l; echo done"]` (fork, execve, pipe, wait4) three-way where host-runnable,
  interp==JIT always. Second test: `$(...)` substitution (vfork path).

### OCI-5 — Sockets / loopback networking (acceptance: guest-to-guest HTTP)

- `x86jit-linux::net`: `socketpair`, AF_UNIX, AF_INET loopback via an in-process
  `NetHub` (port table, connect/accept rendezvous, in-memory streams as OFDs),
  `poll`/`select`/`epoll_*` over OFDs, `eventfd`, `timerfd`, nonblocking, `sendto/
  recvfrom`.
- Acceptance: one image, two guest processes — `busybox httpd` + `busybox wget` over
  guest loopback; stdout diff interp==JIT. Host port bridging + registry `pull` stay
  out (offline-first), recorded as follow-ups.

### OCI-6 — Consolidation (parallel-friendly, low risk)

- `x86jit-bench` image workloads (cold-start + steady-state; the FD-AOT decision
  input); a `python:3-slim`-style image running the same script as `python.rs`
  (closing the loop with the corpus); `status.md`/`commands.md` updates; retire
  waivers or convert to v3 roadmap issues. **v3/AVX is deliberately NOT in this
  track** — it changes `CpuState` XMM→YMM layout (`jit_abi.rs` offsets) and needs the
  non-Unicorn oracle; its own brief when a target image demands it, the compat map
  quantifying exactly how many codes it unlocks.

## 3. Structural safeguards (summary)

- **Boundary**: dep directions enforced by Cargo + `core_stays_guest_agnostic`;
  `x86jit-oci` physically cannot see core.
- **Coverage**: `isa-coverage.md` generated-only; staleness = failing test; CPUID
  machine-checked against it; waivers explicit, issue-linked, expiring.
- **Gaps**: every unknown instruction/syscall → classified structured report; one GH
  issue per family (`gap:insn`/`gap:syscall`, `track:oci`); roadmap stays in issues.
- **Naming**: phases `OCI-0…OCI-6`, tasks `OCI-<n>.T<k>`, branches
  `feat/oci-<n>-<slug>`, one instruction family per PR.
- **Invariants**: interp==JIT==Unicorn on every added instruction; three-way
  preserved via the `reference()` skip; `Blocks(n)`/SMC/M7/tiering untouched (all work
  embedder-side except lift/interp/codegen arms + the audited `Vm` snapshot API);
  offline-first (vendored/digest-pinned fixtures; no network before a dedicated
  `pull` phase).

## 4. First task: OCI-0.T1/T2 acceptance (smallest correct task)

Build the probe harness and land `cpuid_advertises_only_what_lifts` **test-first**:

1. Write the probe + the test. Confirm it **fails on the current tree**, naming:
   CMPXCHG16B advertised (interp.rs leaf-1 ECX bit 13) with `Cmpxchg16b` absent from
   `lift_insn`; MMX advertised (EDX bit 23) with zero MMX lifts; SSE3/SSSE3/SSE4.1/
   SSE4.2 advertised with the specific missing code lists the probe emits.
   (**Verified true**: ecx bits SSE3|SSSE3|CX16|SSE4.1|SSE4.2|POPCNT set; lift has 0
   arms for cmpxchg16b/palignr/ptest/movddup/pmovzx/pmulld.)
2. Resolve per D2/OCI-0.T2 (un-advertise CX16+MMX now; waiver SSE3/SSSE3/SSE4.x vs
   scheduled OCI-3 issues); fix the stale `cpuid_run` doc comment.
3. Commit generated `coverage.json` + `isa-coverage.md`; `compat_map_is_current`
   green; full suite green (the CPUID narrowing must not regress `glibc.rs`/
   `python.rs` — if it does, that is a real finding to keep, not to suppress).

Exact fast-dispatch-R1 shape: a latent, verified inconsistency fixed generically,
producing the substrate (the map) everything else stands on.

## Critical files

- `x86jit-core/src/lift.rs` — the instruction surface (`lift_insn`, `LiftError`); what
  the compat probe measures.
- `x86jit-core/src/interp.rs` — `cpuid_run` (advertised features) + interpreter arms.
- `x86jit-tests/src/syscall.rs` — the 1023-line shim that graduates into
  `x86jit-linux` and grows into `GuestFs`/`Kernel`.
- `x86jit-cranelift/src/codegen.rs` — JIT arms (paired with interp via shared helpers).
- `x86jit-tests/tests/mt.rs` — clone/futex thread harness → multi-process substrate.
