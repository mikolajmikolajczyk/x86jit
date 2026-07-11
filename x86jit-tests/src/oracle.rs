//! The oracle abstraction (testing.md §4). An `Oracle` runs a `VectorInput` and
//! returns a `RunOutcome`; the engine-under-test implements the same shape, so a
//! differential run is `oracle.run(input)` vs `engine.run(input)` compared by the
//! same comparator (§5).
//!
//! `InterpreterOracle` wraps `x86jit-core` — it is both the engine under test
//! (against Unicorn) and, later, the oracle for the JIT (§8).

use x86jit_core::{
    AccessKind, Backend, CpuMode, Exit, GuestCpuFeatures, InterpreterBackend, MemConsistency,
    MemoryModel, PortDir, Prot, Reg, Vm, VmConfig,
};

use crate::vector::{Access, CpuSnapshot, ExitKind, MemChunk, RunSpec};

/// Everything needed to execute, without the expectations.
#[derive(Clone, Debug)]
pub struct VectorInput {
    pub cpu_init: CpuSnapshot,
    pub mem_init: Vec<MemChunk>,
    pub entry: u64,
    pub run: RunSpec,
}

/// What comes out of an execution.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub cpu: CpuSnapshot,
    /// Memory read back over the same regions as `mem_init` (so the comparator can
    /// diff exactly what could have changed).
    pub mem: Vec<MemChunk>,
    pub exit: ExitKind,
}

pub trait Oracle {
    fn run(&self, input: &VectorInput) -> RunOutcome;
    fn name(&self) -> &str;
}

/// Guard against a `RunSpec::UntilExit` snippet with no terminator looping forever.
const UNTIL_EXIT_BUDGET: u64 = 100_000;

/// The engine under test: `x86jit-core`'s interpreter behind the `Oracle` shape.
pub struct InterpreterOracle;

impl Oracle for InterpreterOracle {
    fn name(&self) -> &str {
        "interpreter"
    }

    fn run(&self, input: &VectorInput) -> RunOutcome {
        run_with_backend(input, Box::new(InterpreterBackend))
    }
}

/// The engine under test in 32-bit compat mode (task-197): `x86jit-core`'s
/// interpreter with `CpuMode::Compat32`, the peer of [`UnicornOracle32`] on the
/// 32-bit differential lane.
pub struct InterpreterOracle32;

impl Oracle for InterpreterOracle32 {
    fn name(&self) -> &str {
        "interpreter32"
    }

    fn run(&self, input: &VectorInput) -> RunOutcome {
        run_with_backend_mode(
            input,
            Box::new(InterpreterBackend),
            GuestCpuFeatures::default(),
            CpuMode::Compat32,
        )
    }
}

/// Execute a `VectorInput` on a `Vm` driven by the given backend (interpreter or
/// JIT). The engine-agnostic core of every oracle — differential JIT-vs-interp
/// runs both through here (§8, testing.md §8.1).
pub fn run_with_backend(input: &VectorInput, backend: Box<dyn Backend>) -> RunOutcome {
    run_with_backend_features(input, backend, GuestCpuFeatures::default())
}

/// As [`run_with_backend`], but with an explicit guest CPU feature set (task-169) so a
/// test can advertise a different ISA level (e.g. `GuestCpuFeatures::v4()` for AVX-512).
pub fn run_with_backend_features(
    input: &VectorInput,
    backend: Box<dyn Backend>,
    features: GuestCpuFeatures,
) -> RunOutcome {
    run_with_backend_mode(input, backend, features, CpuMode::Long64)
}

/// As [`run_with_backend`], but selecting the x87 transcendental precision (task-212).
pub fn run_with_backend_x87(
    input: &VectorInput,
    backend: Box<dyn Backend>,
    precision: x86jit_core::state::X87Precision,
) -> RunOutcome {
    let size = flat_size(&input.mem_init);
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.set_x87_precision(precision);
    run_vm(&mut vm, input)
}

/// As [`run_with_backend_features`], but with an explicit guest [`CpuMode`] (task-197).
/// `CpuMode::Compat32` selects the 32-bit differential lane: the same `VectorInput`
/// is decoded/executed under `Vm::set_cpu_mode(Compat32)`, the mode a parameter to
/// the run — not a forked engine.
pub fn run_with_backend_mode(
    input: &VectorInput,
    backend: Box<dyn Backend>,
    features: GuestCpuFeatures,
    mode: CpuMode,
) -> RunOutcome {
    let size = flat_size(&input.mem_init);
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.set_guest_cpu_features(features);
    vm.set_cpu_mode(mode);
    run_vm(&mut vm, input)
}

/// Map the input, spawn a vcpu, run to the budget, and snapshot the outcome. Shared by
/// the configured `run_with_backend_*` entry points.
fn run_vm(vm: &mut Vm, input: &VectorInput) -> RunOutcome {
    // bg-tier sweep (BGT-3 AC#3): `X86JIT_BG_TIER=1` runs the whole corpus under
    // background tier-up, off by default so the standard runs are untouched (AC#4).
    // Harmless for the interpreter backend — its `tier_up_async` returns
    // `Unsupported`, so a hot block just falls through to another interpreted block.
    if std::env::var_os("X86JIT_BG_TIER").is_some() {
        vm.set_tier_up_after(Some(2));
        vm.set_tier_up_background(true);
    }

    for chunk in &input.mem_init {
        vm.map(chunk.addr, chunk.bytes.len(), Prot::RWX, chunk.kind.into())
            .expect("vector region maps within the flat buffer");
        vm.write_bytes(chunk.addr, &chunk.bytes)
            .expect("vector bytes fit the mapped region");
    }

    let mut cpu = vm.new_vcpu();
    load_snapshot(&mut cpu, &input.cpu_init, input.entry);

    let budget = match input.run {
        RunSpec::Blocks(n) => Some(n),
        RunSpec::UntilExit => Some(UNTIL_EXIT_BUDGET),
    };
    let exit = cpu.run(&*vm, budget);

    RunOutcome {
        cpu: store_snapshot(&cpu),
        mem: read_back(vm, &input.mem_init),
        exit: exit_kind(&exit),
    }
}

/// Smallest page-rounded flat size covering all chunks.
fn flat_size(chunks: &[MemChunk]) -> u64 {
    let end = chunks
        .iter()
        .map(|c| c.addr + c.bytes.len() as u64)
        .max()
        .unwrap_or(0);
    (end + 0xfff) & !0xfff
}

fn load_snapshot(cpu: &mut x86jit_core::Vcpu, snap: &CpuSnapshot, entry: u64) {
    for (i, &v) in snap.gpr.iter().enumerate() {
        cpu.set_reg(Reg::from_gpr_index(i), v);
    }
    cpu.set_reg(Reg::FsBase, snap.fs_base);
    cpu.set_reg(Reg::GsBase, snap.gs_base);
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_flags(snap.flags.into());
    for (i, &v) in snap.xmm.iter().enumerate() {
        cpu.set_xmm(i, v);
    }
    for (i, &v) in snap.ymm_hi.iter().enumerate() {
        cpu.set_ymm_hi(i, v);
    }
    for (i, &[lo, hi]) in snap.zmm_hi.iter().enumerate() {
        cpu.set_zmm_hi(i, 0, lo);
        cpu.set_zmm_hi(i, 1, hi);
    }
    for (i, &v) in snap.kmask.iter().enumerate() {
        cpu.set_kmask(i, v);
    }
    // x87 (task-188): seed the control word so both engines start from the same
    // (hardware-reset) value, then load the stack in architectural order — `st[i]`
    // is `ST(i)`, which lives at physical `fpr[(top + i) & 7]`.
    cpu.set_fpu_cw(snap.fpu_cw);
    cpu.set_fpu_top(snap.fpu_top as u32);
    let top = snap.fpu_top as u32;
    for (i, bytes) in snap.st.iter().enumerate() {
        let phys = ((top + i as u32) & 7) as usize;
        cpu.set_fpr_bytes(phys, bytes);
    }
}

fn store_snapshot(cpu: &x86jit_core::Vcpu) -> CpuSnapshot {
    let mut gpr = [0u64; 16];
    for (i, slot) in gpr.iter_mut().enumerate() {
        *slot = cpu.reg(Reg::from_gpr_index(i));
    }
    let mut xmm = [0u128; 16];
    for (i, slot) in xmm.iter_mut().enumerate() {
        *slot = cpu.xmm(i);
    }
    let mut ymm_hi = [0u128; 16];
    for (i, slot) in ymm_hi.iter_mut().enumerate() {
        *slot = cpu.ymm_hi(i);
    }
    let mut zmm_hi = [[0u128; 2]; 16];
    for (i, slot) in zmm_hi.iter_mut().enumerate() {
        *slot = [cpu.zmm_hi(i, 0), cpu.zmm_hi(i, 1)];
    }
    let mut kmask = [0u64; 8];
    for (i, slot) in kmask.iter_mut().enumerate() {
        *slot = cpu.kmask(i);
    }
    // x87 (task-188): de-rotate the physical `fpr[]` into architectural ST order so
    // `st[i]` is `ST(i)` = `fpr[(top + i) & 7]`, matching Unicorn's ST0..ST7.
    let top = cpu.fpu_top();
    let mut st = [[0u8; 10]; 8];
    for (i, slot) in st.iter_mut().enumerate() {
        let phys = ((top + i as u32) & 7) as usize;
        *slot = cpu.fpr_bytes(phys);
    }
    CpuSnapshot {
        gpr,
        rip: cpu.reg(Reg::Rip),
        flags: cpu.flags().into(),
        fs_base: cpu.reg(Reg::FsBase),
        gs_base: cpu.reg(Reg::GsBase),
        xmm,
        ymm_hi,
        zmm_hi,
        kmask,
        st,
        fpu_cw: cpu.fpu_cw(),
        fpu_top: (top & 7) as u8,
    }
}

fn read_back(vm: &Vm, chunks: &[MemChunk]) -> Vec<MemChunk> {
    chunks
        .iter()
        .map(|c| {
            let mut bytes = vec![0u8; c.bytes.len()];
            vm.read_bytes(c.addr, &mut bytes)
                .expect("region still mapped");
            MemChunk {
                addr: c.addr,
                bytes,
                kind: c.kind,
            }
        })
        .collect()
}

fn exit_kind(exit: &Exit) -> ExitKind {
    match exit {
        Exit::Hlt => ExitKind::Hlt,
        Exit::Syscall => ExitKind::Syscall,
        Exit::UnmappedMemory { addr, access } => ExitKind::UnmappedMemory {
            addr: *addr,
            access: access_kind(*access),
        },
        Exit::MmioRead { addr, size } => ExitKind::MmioRead {
            addr: *addr,
            size: *size,
        },
        Exit::MmioWrite { addr, size, value } => ExitKind::MmioWrite {
            addr: *addr,
            size: *size,
            value: *value,
        },
        Exit::UnknownInstruction { addr, .. } => ExitKind::UnknownInstruction { addr: *addr },
        Exit::Exception { addr, vector } => ExitKind::Exception {
            addr: *addr,
            vector: *vector,
        },
        Exit::PortIo {
            port,
            size,
            dir,
            value,
        } => ExitKind::PortIo {
            port: *port,
            size: *size,
            out: *dir == PortDir::Out,
            value: *value,
        },
        Exit::BudgetExhausted => ExitKind::Budget,
    }
}

fn access_kind(a: AccessKind) -> Access {
    match a {
        AccessKind::Read => Access::Read,
        AccessKind::Write => Access::Write,
        AccessKind::Execute => Access::Execute,
    }
}
