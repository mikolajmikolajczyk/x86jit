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
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
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

/// One benchmark case. `guest` runs it on a given backend (loading, compiling and
/// executing — the whole end-to-end wall clock is what the caller times, so JIT
/// compile cost is included, which is the honest one-shot number). `native`, when
/// present, runs the real binary as a host subprocess.
pub struct Workload {
    pub name: &'static str,
    pub kind: &'static str,
    pub guest: fn(Box<dyn Backend>) -> (Vec<u8>, Counters),
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
    ]
}

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

fn run_guest(image: &[u8], cfg: &GuestCfg, backend: Box<dyn Backend>) -> (Vec<u8>, Counters) {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: cfg.flat },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.set_tier_up_after(tier_from_env());
    vm.set_tier_up_background(bg_from_env());
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

fn guest_sha256(backend: Box<dyn Backend>) -> (Vec<u8>, Counters) {
    let cfg = GuestCfg {
        flat: 0x80_0000,
        heap_base: 0x50_0000,
        mmap_base: None,
        stack_top: 0x70_0000,
        argv: &[b"sha256"],
        env: &[],
    };
    run_guest(SHA256_ELF, &cfg, backend)
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

fn guest_sqlite(backend: Box<dyn Backend>) -> (Vec<u8>, Counters) {
    let cfg = GuestCfg {
        flat: 0x400_0000,
        heap_base: 0x70_0000,
        mmap_base: Some(0x100_0000),
        stack_top: 0x3f0_0000,
        argv: &[b"sqlite3", b":memory:", SQL.as_bytes()],
        env: &[b"PATH=/bin"],
    };
    run_guest(SQLITE_ELF, &cfg, backend)
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

fn guest_lua(backend: Box<dyn Backend>) -> (Vec<u8>, Counters) {
    let cfg = GuestCfg {
        flat: 0x400_0000,
        heap_base: 0x60_0000,
        mmap_base: Some(0x100_0000),
        stack_top: 0x3f0_0000,
        argv: &[b"lua", b"-e", LUA_SCRIPT.as_bytes()],
        env: &[b"PATH=/bin"],
    };
    run_guest(LUA_ELF, &cfg, backend)
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

fn guest_fib32(backend: Box<dyn Backend>) -> (Vec<u8>, Counters) {
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

    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: 0x10_0000 },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.set_tier_up_after(tier_from_env());
    vm.set_tier_up_background(bg_from_env());
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

/// Hotness tier threshold from `X86JIT_TIER` (experiment knob), else eager.
fn tier_from_env() -> Option<u32> {
    std::env::var("X86JIT_TIER")
        .ok()
        .and_then(|s| s.parse().ok())
}

/// Background tier-up on/off from `X86JIT_BG_TIER` (bg-tier BGT-5 experiment knob).
/// Only meaningful with `X86JIT_TIER` set and the JIT backend.
fn bg_from_env() -> bool {
    std::env::var_os("X86JIT_BG_TIER").is_some()
}

/// A fresh interpreter backend (helper for the caller).
pub fn interp() -> Box<dyn Backend> {
    Box::new(InterpreterBackend)
}

/// A fresh JIT backend (helper for the caller).
pub fn jit() -> Box<dyn Backend> {
    Box::new(JitBackend::new())
}
