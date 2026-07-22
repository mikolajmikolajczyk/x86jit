//! Benchmark workloads run three ways (native subprocess, interpreter, JIT).
//!
//! Two ends of the spectrum on purpose (see the fast-dispatch track, §12):
//! - **dispatch-bound micro** (`fib`) — tiny blocks, maximal transfer pressure;
//! - **compute-hot** (`sha256`) — a long scalar loop where JIT compile amortizes;
//! - **one-shot startup** (`sqlite`, `lua`) — large real apps run once, where
//!   Cranelift's per-block compile cost dominates the wall clock.
//!
//! The guest ELFs are the same fixtures the whole-program tests use. Each workload
//! also carries its expected output so the bench doubles as a correctness gate
//! (native == interpreter == JIT).

use x86jit_core::{
    Backend, Exit, InterpreterBackend, Prot, Reg, RegionCaps, RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::{HostTarget, JitBackend, OptLevel};
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::syscall::LinuxShim;

/// Fast-dispatch counters captured from a JIT run (all zero for the interpreter).
#[derive(Clone, Copy, Default)]
pub struct Counters {
    pub chained: u64,
    pub ibtc_filled: u64,
    pub fast_hits: u64,
    pub misses: u64,
    /// Total time spent compiling during the run (perf-bench v2 PB-2). Zero for the
    /// interpreter; the JIT's `Backend::compile_ns`. Lets the bench split the JIT
    /// wall clock into compile vs steady-state execute.
    pub compile_ns: u64,
}

/// Tier-up configuration for one measured run (perf-bench v2 tiering modes): which
/// mode the guest's `Vm` is set to. The bench measures each workload across
/// [`EAGER`](TierCfg::EAGER) / [`tier`](TierCfg::tier) / [`bg`](TierCfg::bg) so the
/// recorded table shows the real deployment picture, not just eager compilation.
#[derive(Clone, Copy)]
pub struct TierCfg {
    pub after: Option<u32>,
    pub background: bool,
    /// Adaptive region threshold T2 (task-156): a hot loop tiers to a region only after
    /// this many executions (≫ `after`). `None` → region at T1 (pre-156 behavior).
    pub region_after: Option<u32>,
}

impl TierCfg {
    /// Compile every block on first execution (no tiering) — the honest one-shot cost.
    pub const EAGER: TierCfg = TierCfg {
        after: None,
        background: false,
        region_after: None,
    };
    /// Interpret each block until `n` executions, then JIT-compile it inline (FD-TIER).
    pub fn tier(n: u32) -> TierCfg {
        TierCfg {
            after: Some(n),
            background: false,
            region_after: None,
        }
    }
    /// Like [`tier`](TierCfg::tier) but compile on the backend's worker thread (bg-tier).
    pub fn bg(n: u32) -> TierCfg {
        TierCfg {
            after: Some(n),
            background: true,
            region_after: None,
        }
    }
    fn apply(&self, vm: &mut Vm) {
        vm.set_tier_up_after(self.after);
        vm.set_tier_up_background(self.background);
        vm.set_tier_up_region_after(self.region_after);
    }
}

/// Runs a workload on a given backend + tier config, returning its output and JIT
/// counters. The whole end-to-end wall clock is what the caller times.
pub type GuestFn = fn(Box<dyn Backend>, TierCfg) -> (Vec<u8>, Counters);

/// One benchmark case. `native`, when present, runs the real binary as a host
/// subprocess.
pub struct Workload {
    pub name: &'static str,
    pub kind: &'static str,
    pub guest: GuestFn,
    pub native: Option<fn() -> Vec<u8>>,
    pub expect: &'static [u8],
}

pub fn all() -> Vec<Workload> {
    vec![
        Workload {
            name: "fib32",
            kind: "dispatch-micro",
            guest: guest_fib32,
            native: None, // hand-assembled snippet, no host binary to exec
            expect: b"fib32=2178309",
        },
        Workload {
            name: "sha256",
            kind: "compute-hot",
            guest: guest_sha256,
            native: Some(native_sha256),
            expect: SHA256_DIGEST,
        },
        Workload {
            name: "sqlite",
            kind: "one-shot",
            guest: guest_sqlite,
            native: Some(native_sqlite),
            expect: b"385|10|100\n",
        },
        Workload {
            name: "lua",
            kind: "one-shot",
            guest: guest_lua,
            native: Some(native_lua),
            expect: b"ok\txxx\n",
        },
        Workload {
            // A long multi-block warm loop — the case superblock regions win (BGT-6):
            // its `region-bg` column beats the single-block modes, which the one-shot
            // workloads above never do. See superblock-plan.md T3f.
            name: "hotloop",
            kind: "warm-loop",
            guest: guest_hotloop_wl,
            native: None, // hand-assembled snippet, no host binary to exec
            expect: HOTLOOP_EXPECT,
        },
        // Game-shaped kernels (task-235): the SIMD / streaming / indirect-dispatch
        // shapes real games hammer, which the corpus above does not exercise.
        Workload {
            name: "simd",
            kind: "simd-hot",
            guest: guest_simd_float,
            native: None,
            expect: SIMD_EXPECT,
        },
        Workload {
            name: "memcpy",
            kind: "stream",
            guest: guest_memcpy,
            native: None,
            expect: MEMCPY_EXPECT,
        },
        Workload {
            name: "indirect",
            kind: "indirect",
            guest: guest_indirect,
            native: None,
            expect: INDIRECT_EXPECT,
        },
    ]
}

/// Deterministic `eax` of [`guest_hotloop`] at [`HOTLOOP_N`], as its text output.
const HOTLOOP_EXPECT: &[u8] = b"hotloop=4063431766";

// --- guest ELF plumbing (shared with the whole-program tests' setup) ---

/// Per-program guest memory map + process args.
struct GuestCfg<'a> {
    flat: u64,
    heap_base: u64,
    /// `Some` when the program uses the mmap arena (glibc/musl allocators); the
    /// heap grows up to it and mmap serves from it.
    mmap_base: Option<u64>,
    stack_top: u64,
    argv: &'a [&'a [u8]],
    env: &'a [&'a [u8]],
}

fn run_guest(
    image: &[u8],
    cfg: &GuestCfg,
    backend: Box<dyn Backend>,
    tier: TierCfg,
) -> (Vec<u8>, Counters) {
    let mut vm = Vm::with_backend(VmConfig::flat(cfg.flat), backend);
    tier.apply(&mut vm);
    let entry = load_static_elf(&mut vm, image).expect("load elf");
    // One RW block from the heap base to the top of the image covers heap, mmap
    // arena and stack (they all live below `flat`).
    vm.map(
        cfg.heap_base,
        (cfg.flat - cfg.heap_base) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();
    let sp = setup_stack(&mut vm, cfg.stack_top, cfg.argv, cfg.env).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, sp);

    let mut shim = LinuxShim::new();
    shim.brk = cfg.heap_base;
    match cfg.mmap_base {
        Some(mb) => {
            shim.brk_limit = mb;
            shim.mmap_base = mb;
            shim.mmap_limit = cfg.stack_top - 0x10_0000;
        }
        None => shim.brk_limit = cfg.stack_top,
    }
    loop {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &vm) {
                    break;
                }
            }
            other => panic!("gap at rip={:#x}: {other:?}", cpu.reg(Reg::Rip)),
        }
    }
    let counters = Counters {
        chained: vm.cache.chained(),
        ibtc_filled: vm.cache.ibtc_filled(),
        fast_hits: cpu.fast_hits(),
        misses: vm.cache.misses(),
        compile_ns: vm.backend.compile_ns(),
    };
    (shim.stdout, counters)
}

// --- sha256 (compute-hot: 5000 hash iterations, freestanding) ---

const SHA256_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-tests/programs/sha256.elf"
));
const SHA256_DIGEST: &[u8] = x86jit_tests::SHA256_FIXTURE_DIGEST;

fn guest_sha256(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    let cfg = GuestCfg {
        flat: 0x80_0000,
        heap_base: 0x50_0000,
        mmap_base: None,
        stack_top: 0x70_0000,
        argv: &[b"sha256"],
        env: &[],
    };
    run_guest(SHA256_ELF, &cfg, backend, tier)
}

fn native_sha256() -> Vec<u8> {
    std::process::Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../x86jit-tests/programs/sha256.elf"
    ))
    .output()
    .expect("run native sha256")
    .stdout
}

// --- sqlite (one-shot: in-memory recursive query) ---

const SQLITE_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-tests/programs/sqlite3.elf"
));
const SQL: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<10) \
                   SELECT sum(x*x), count(x), max(x*x) FROM c;";

fn guest_sqlite(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    let cfg = GuestCfg {
        flat: 0x400_0000,
        heap_base: 0x70_0000,
        mmap_base: Some(0x100_0000),
        stack_top: 0x3f0_0000,
        argv: &[b"sqlite3", b":memory:", SQL.as_bytes()],
        env: &[b"PATH=/bin"],
    };
    run_guest(SQLITE_ELF, &cfg, backend, tier)
}

fn native_sqlite() -> Vec<u8> {
    std::process::Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../x86jit-tests/programs/sqlite3.elf"
    ))
    .args([":memory:", SQL])
    .output()
    .expect("run native sqlite3")
    .stdout
}

// --- lua (one-shot: tables, ipairs, float math) ---

const LUA_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-tests/programs/lua.elf"
));
const LUA_SCRIPT: &str = "local t={} for i=1,100 do t[i]=i*i end \
                          local s=0 for _,v in ipairs(t) do s=s+v end \
                          local ok = (s==338350) and (math.sqrt(2)>1.41 and math.sqrt(2)<1.42) \
                          print(ok and 'ok' or 'bad', string.rep('x',3))";

fn guest_lua(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    let cfg = GuestCfg {
        flat: 0x400_0000,
        heap_base: 0x60_0000,
        mmap_base: Some(0x100_0000),
        stack_top: 0x3f0_0000,
        argv: &[b"lua", b"-e", LUA_SCRIPT.as_bytes()],
        env: &[b"PATH=/bin"],
    };
    run_guest(LUA_ELF, &cfg, backend, tier)
}

fn native_lua() -> Vec<u8> {
    std::process::Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../x86jit-tests/programs/lua.elf"
    ))
    .args(["-e", LUA_SCRIPT])
    .output()
    .expect("run native lua")
    .stdout
}

// --- go-startup (bg-tier BGT-5: startup-heavy Go over the threaded driver) ---

/// A static Go "hello" run over the threaded driver + Reserved span, the go-caddy
/// layout. Startup-dominated (the Go runtime touches thousands of run-once blocks),
/// so it's the workload where eager compile hurts most and tier-up / background
/// tier-up help most. `tier`/`background` are passed explicitly (not via env) since
/// this runs outside the `all()` corpus. Returns stdout ("hello from go stdout\n").
pub fn go_startup(backend: Box<dyn Backend>, tier: Option<u32>, background: bool) -> Vec<u8> {
    use x86jit_tests::guest::Guest;
    const GO_SPAN: u64 = 1 << 40;
    const HEAP_BASE: u64 = 0x100_0000;
    const BRK_LIMIT: u64 = 0x180_0000;
    const STACK_TOP: u64 = 0x8000_0000;
    const MMAP_BASE: u64 = 0x1_0000_0000;
    const MMAP_LIMIT: u64 = MMAP_BASE + (512 << 30);
    let mut g = Guest::new_static(GO_HELLO_ELF)
        .reserved(GO_SPAN)
        .heap_base(HEAP_BASE)
        .brk_limit(BRK_LIMIT)
        .mmap_base(MMAP_BASE)
        .mmap_limit(MMAP_LIMIT)
        .stack_top(STACK_TOP)
        .argv(&[b"hello_go"])
        .tier_up(tier);
    if background {
        g = g.tier_up_background();
    }
    g.run_threaded(backend)
}

const GO_HELLO_ELF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../x86jit-tests/programs/hello_go.elf"
));

/// Expected stdout of the Go hello workload.
pub const GO_HELLO_OUT: &[u8] = b"hello from go stdout\n";

// --- fib32 (dispatch-bound micro: naive recursive Fibonacci) ---

fn guest_fib32(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    use iced_x86::code_asm::*;
    const CODE: u64 = 0x1000;
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut fib = asm.create_label();
    let mut base = asm.create_label();
    asm.mov(edi, 32i32).unwrap();
    asm.call(fib).unwrap();
    asm.hlt().unwrap();
    asm.set_label(&mut fib).unwrap();
    asm.cmp(edi, 2i32).unwrap();
    asm.jb(base).unwrap();
    asm.push(rdi).unwrap();
    asm.sub(edi, 1i32).unwrap();
    asm.call(fib).unwrap();
    asm.pop(rdi).unwrap();
    asm.push(rax).unwrap();
    asm.sub(edi, 2i32).unwrap();
    asm.call(fib).unwrap();
    asm.pop(rcx).unwrap();
    asm.add(eax, ecx).unwrap();
    asm.ret().unwrap();
    asm.set_label(&mut base).unwrap();
    asm.mov(eax, edi).unwrap();
    asm.ret().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x10_0000), backend);
    tier.apply(&mut vm);
    vm.map(0, 0x10_0000, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rsp, 0x8_0000);
    match cpu.run(&vm, None) {
        Exit::Hlt => {}
        other => panic!("fib exited unexpectedly: {other:?}"),
    }
    let out = format!("fib32={}", cpu.reg(Reg::Rax) as u32).into_bytes();
    let counters = Counters {
        chained: vm.cache.chained(),
        ibtc_filled: vm.cache.ibtc_filled(),
        fast_hits: cpu.fast_hits(),
        misses: vm.cache.misses(),
        compile_ns: vm.backend.compile_ns(),
    };
    (out, counters)
}

/// A long-running **multi-block** hot loop (BGT-6 region-favorable): each iteration
/// branches (so the loop body is several blocks → `lift_region` forms a region), and it
/// runs `iters` times — long enough to reach the warm regime where a region's
/// no-inter-block-dispatch + register-carried execution amortizes its heavier compile.
/// The single-block modes must chain/dispatch between the body's blocks every iteration;
/// the region runs them as one function. Deterministic `eax`, returned as text.
pub fn guest_hotloop(backend: Box<dyn Backend>, tier: TierCfg, iters: u32) -> (Vec<u8>, Counters) {
    use iced_x86::code_asm::*;
    const CODE: u64 = 0x1000;
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    let mut quad = asm.create_label();
    let mut cont = asm.create_label();
    asm.xor(eax, eax).unwrap(); // acc
    asm.mov(ecx, iters as i32).unwrap(); // counter
    asm.set_label(&mut top).unwrap();
    asm.test(ecx, 3i32).unwrap(); // branch inside the loop → multi-block body
    asm.jz(quad).unwrap();
    asm.add(eax, ecx).unwrap(); // 3-of-4 iterations
    asm.xor(edx, edx).unwrap();
    asm.jmp(cont).unwrap();
    asm.set_label(&mut quad).unwrap();
    asm.imul_2(eax, eax).unwrap(); // every 4th: mix it up
    asm.add(eax, 7i32).unwrap();
    asm.set_label(&mut cont).unwrap();
    asm.dec(ecx).unwrap();
    asm.jnz(top).unwrap(); // back-edge → loop
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x10_0000), backend);
    tier.apply(&mut vm);
    vm.map(0, 0x10_0000, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rsp, 0x8_0000);
    loop {
        match cpu.run(&vm, None) {
            Exit::Hlt => break,
            Exit::BudgetExhausted => continue,
            other => panic!("hotloop exited unexpectedly: {other:?}"),
        }
    }
    let out = format!("hotloop={}", cpu.reg(Reg::Rax) as u32).into_bytes();
    let counters = Counters {
        chained: vm.cache.chained(),
        ibtc_filled: vm.cache.ibtc_filled(),
        fast_hits: cpu.fast_hits(),
        misses: vm.cache.misses(),
        compile_ns: vm.backend.compile_ns(),
    };
    (out, counters)
}

/// Iteration count for the recorded `hotloop` workload — long enough for a region's
/// one-time compile to amortize into a clear warm-loop win, short enough that the
/// interpreter leg stays gate-friendly.
const HOTLOOP_N: u32 = 4_000_000;

/// `all()` adapter for [`guest_hotloop`] at [`HOTLOOP_N`] (fixed-`iters` `GuestFn`).
fn guest_hotloop_wl(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    guest_hotloop(backend, tier, HOTLOOP_N)
}

// --- game-shaped kernels (task-235): SIMD-float, memcpy bandwidth, indirect dispatch ---
//
// Games are hot-loop + heavy-SIMD + draw-call (indirect) shaped. The fib/sha/one-shot
// corpus above does not exercise those; these three do, as freestanding hand-assembled
// snippets (like `fib32`/`hotloop` — no host binary, so `native: None`). Each is fully
// deterministic (integer or bit-exact IEEE), so it doubles as an interp==JIT gate. The
// golden `expect` bytes come from `x86jit-bench dump` (a run of the interpreter leg).

/// Common origin for the hand-assembled kernels below (dispatcher / loop body).
const KCODE: u64 = 0x1000;

/// Assemble-and-run a freestanding snippet in a flat 1 MiB RW guest: writes `data`
/// spans, then `code` at [`KCODE`], runs to `hlt`, and returns `eax` + JIT counters.
/// The RW map is executable (the guest model doesn't NX `Ram`), matching `fib32`.
fn run_code(
    code: &[u8],
    data: &[(u64, Vec<u8>)],
    backend: Box<dyn Backend>,
    tier: TierCfg,
) -> (u32, Counters) {
    let mut vm = Vm::with_backend(VmConfig::flat(0x10_0000), backend);
    tier.apply(&mut vm);
    vm.map(0, 0x10_0000, Prot::RW, RegionKind::Ram).unwrap();
    for (addr, bytes) in data {
        vm.write_bytes(*addr, bytes).unwrap();
    }
    vm.write_bytes(KCODE, code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, KCODE);
    cpu.set_reg(Reg::Rsp, 0x8_0000);
    loop {
        match cpu.run(&vm, None) {
            Exit::Hlt => break,
            Exit::BudgetExhausted => continue,
            other => panic!("kernel exited unexpectedly: {other:?}"),
        }
    }
    let counters = Counters {
        chained: vm.cache.chained(),
        ibtc_filled: vm.cache.ibtc_filled(),
        fast_hits: cpu.fast_hits(),
        misses: vm.cache.misses(),
        compile_ns: vm.backend.compile_ns(),
    };
    (cpu.reg(Reg::Rax) as u32, counters)
}

// --- simd_float (SIMD-hot: packed-single lerp/damp accumulator, game vec4 math) ---

/// Iterations of the packed-float accumulator — two packed ops each, long enough for a
/// clear SIMD codegen signal, short enough that the interpreter leg stays gate-friendly.
const SIMD_N: u32 = 1_000_000;
const SIMD_EXPECT: &[u8] = b"simd=411fedb2";

/// A damped packed-single accumulator: `acc = acc*c1 + c2` per iteration over 4 lanes
/// (`mulps`+`addps`), the shape of a particle-SoA integrate / vec4 transform inner loop.
/// `c1 = 0.9999`, `c2 = [1,2,3,4]*1e-4`, so `acc` converges (bounded, no inf/nan). Final:
/// horizontal-sum the 4 lanes (`shufps`+`addps`/`addss`) and emit lane-0 bits as hex.
fn guest_simd_float(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    use iced_x86::code_asm::*;
    const C1: u64 = 0x5000; // [0.9999; 4]
    const C2: u64 = 0x5010; // [1,2,3,4] * 1e-4
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    asm.xorps(xmm0, xmm0).unwrap(); // acc = 0
    asm.movups(xmm1, xmmword_ptr(C1)).unwrap();
    asm.movups(xmm2, xmmword_ptr(C2)).unwrap();
    asm.mov(ecx, SIMD_N as i32).unwrap();
    asm.set_label(&mut top).unwrap();
    asm.mulps(xmm0, xmm1).unwrap();
    asm.addps(xmm0, xmm2).unwrap();
    asm.dec(ecx).unwrap();
    asm.jnz(top).unwrap();
    // Horizontal sum of the 4 lanes into lane 0.
    asm.movaps(xmm1, xmm0).unwrap();
    asm.shufps(xmm1, xmm1, 0x4E).unwrap(); // [x2,x3,x0,x1]
    asm.addps(xmm0, xmm1).unwrap(); // lane0=x0+x2, lane1=x1+x3
    asm.movaps(xmm1, xmm0).unwrap();
    asm.shufps(xmm1, xmm1, 0x01).unwrap(); // lane0 <- lane1
    asm.addss(xmm0, xmm1).unwrap(); // lane0 = full sum
    asm.movd(eax, xmm0).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(KCODE).unwrap();

    let c1 = 0.9999f32.to_bits();
    let mut c1b = Vec::with_capacity(16);
    for _ in 0..4 {
        c1b.extend_from_slice(&c1.to_le_bytes());
    }
    let mut c2b = Vec::with_capacity(16);
    for i in 1..=4u32 {
        c2b.extend_from_slice(&(i as f32 * 1e-4).to_bits().to_le_bytes());
    }
    let (val, c) = run_code(&code, &[(C1, c1b), (C2, c2b)], backend, tier);
    (format!("simd={val:08x}").into_bytes(), c)
}

// --- memcpy (streaming bandwidth: aligned 16-byte copy + checksum fold) ---

const MEMCPY_N: u32 = 1_000_000;
const MEMCPY_BUF: u64 = 0x8000; // 32 KiB, cycled through
const MEMCPY_EXPECT: &[u8] = b"memcpy=0096ce00";

/// Streaming copy: each iteration copies one 16-byte chunk src→dst (`movaps`) and folds
/// the chunk's low dword into a running checksum, cycling through a 32 KiB buffer. The
/// game shape is asset/vertex streaming; the checksum makes the output deterministic and
/// forces the loads not to be dead-code-eliminated.
fn guest_memcpy(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    use iced_x86::code_asm::*;
    const SRC: u64 = 0x2_0000;
    const DST: u64 = 0x3_0000;
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    asm.xor(r8d, r8d).unwrap(); // checksum
    asm.xor(edx, edx).unwrap(); // byte offset into buffer
    asm.mov(esi, SRC as i32).unwrap();
    asm.mov(edi, DST as i32).unwrap();
    asm.mov(ecx, MEMCPY_N as i32).unwrap();
    asm.set_label(&mut top).unwrap();
    asm.movaps(xmm0, xmmword_ptr(rsi + rdx)).unwrap();
    asm.movaps(xmmword_ptr(rdi + rdx), xmm0).unwrap();
    asm.movd(eax, xmm0).unwrap();
    asm.add(r8d, eax).unwrap(); // fold checksum
    asm.add(edx, 16i32).unwrap();
    asm.and(edx, (MEMCPY_BUF as i32) - 16).unwrap(); // wrap, stay 16-aligned
    asm.dec(ecx).unwrap();
    asm.jnz(top).unwrap();
    asm.mov(eax, r8d).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(KCODE).unwrap();

    // Deterministic source pattern; dst starts zero (fresh RAM).
    let src: Vec<u8> = (0..MEMCPY_BUF)
        .map(|i| (i.wrapping_mul(31)) as u8)
        .collect();
    let (val, c) = run_code(&code, &[(SRC, src)], backend, tier);
    (format!("memcpy={val:08x}").into_bytes(), c)
}

// --- indirect (draw-call / vtable dispatch: computed indirect calls, IBTC stress) ---

const INDIRECT_N: u32 = 1_000_000;
const INDIRECT_M: u64 = 16; // leaf count (power of two)
const INDIRECT_LEAVES: u64 = 0x4000; // leaf table base, 8-byte stride
const INDIRECT_EXPECT: &[u8] = b"indirect=03307070";

/// Vtable-style dispatch: an LCG picks one of [`INDIRECT_M`] leaf functions each
/// iteration and `call`s it indirectly (`call r10`, target = base + idx*8). Each leaf is
/// `add eax, imm; ret`, so the accumulator threads through the call. This is the draw-call
/// / virtual-dispatch shape games hammer, and the per-site IBTC's stress case.
fn guest_indirect(backend: Box<dyn Backend>, tier: TierCfg) -> (Vec<u8>, Counters) {
    use iced_x86::code_asm::*;
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    asm.xor(eax, eax).unwrap(); // accumulator (leaves add into it)
    asm.mov(ecx, INDIRECT_N as i32).unwrap(); // iteration counter
    asm.mov(edx, 1i32).unwrap(); // LCG state
    asm.mov(r9, INDIRECT_LEAVES).unwrap(); // leaf base
    asm.set_label(&mut top).unwrap();
    asm.imul_3(edx, edx, 1103515245i32).unwrap(); // LCG step
    asm.add(edx, 12345i32).unwrap();
    asm.mov(r8d, edx).unwrap();
    asm.shr(r8d, 16i32).unwrap();
    asm.and(r8d, (INDIRECT_M as i32) - 1).unwrap(); // idx in [0, M)
    asm.lea(r10, qword_ptr(r9 + r8 * 8)).unwrap(); // target = base + idx*8
    asm.call(r10).unwrap();
    asm.dec(ecx).unwrap();
    asm.jnz(top).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(KCODE).unwrap();

    // Leaf table: leaf i at base+i*8 = `add eax, (i*7+1); ret` padded to 8 bytes.
    let mut leaves = Vec::with_capacity((INDIRECT_M * 8) as usize);
    for i in 0..INDIRECT_M {
        leaves.push(0x05); // add eax, imm32
        leaves.extend_from_slice(&((i * 7 + 1) as u32).to_le_bytes());
        leaves.push(0xC3); // ret
        leaves.push(0x90); // nop pad
        leaves.push(0x90);
    }
    let (val, c) = run_code(&code, &[(INDIRECT_LEAVES, leaves)], backend, tier);
    (format!("indirect={val:08x}").into_bytes(), c)
}

/// A fresh interpreter backend (helper for the caller).
pub fn interp() -> Box<dyn Backend> {
    Box::new(InterpreterBackend)
}

/// Cranelift mid-end level for a run under `tier` (task-276). Derived from the tier-up
/// policy so each column measures what that deployment actually gets: the eager column
/// pays no mid-end (every block compiled once), the tiered columns do. Overridable via
/// `X86JIT_OPT_LEVEL`, parsed here at the edge rather than inside the library (task-181).
fn opt_level(tier: TierCfg) -> OptLevel {
    std::env::var("X86JIT_OPT_LEVEL")
        .ok()
        .and_then(|s| OptLevel::parse(&s))
        .unwrap_or_else(|| OptLevel::for_tiering(tier.after.is_some()))
}

/// Executed-instruction accounting for this bench run, from `X86JIT_ICOUNT=1`
/// (task-281). Off by default so the recorded baseline measures the shipped
/// configuration; on, it costs a load/add/store per guest block.
fn icount_on() -> bool {
    std::env::var_os("X86JIT_ICOUNT").as_deref() == Some(std::ffi::OsStr::new("1"))
}

/// A fresh JIT backend for a run under `tier` (helper for the caller).
pub fn jit(tier: TierCfg) -> Box<dyn Backend> {
    let b = JitBackend::with_opt_level(opt_level(tier));
    if icount_on() {
        b.enable_icount();
    }
    Box::new(b)
}

/// A region-forming JIT backend (BGT-6): with `TierCfg::bg`, hot loops tier up to
/// background-compiled superblock regions. Caps mirror the superblock tests / runner.
pub fn jit_regions(tier: TierCfg) -> Box<dyn Backend> {
    let b = JitBackend::with_options(
        Some(RegionCaps {
            max_blocks: 16,
            max_icount: 256,
        }),
        HostTarget::Native,
        opt_level(tier),
    );
    if icount_on() {
        b.enable_icount();
    }
    Box::new(b)
}
