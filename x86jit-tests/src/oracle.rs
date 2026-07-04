//! The oracle abstraction (testing.md §4). An `Oracle` runs a `VectorInput` and
//! returns a `RunOutcome`; the engine-under-test implements the same shape, so a
//! differential run is `oracle.run(input)` vs `engine.run(input)` compared by the
//! same comparator (§5).
//!
//! `InterpreterOracle` wraps `x86jit-core` — it is both the engine under test
//! (against Unicorn) and, later, the oracle for the JIT (§8).

use x86jit_core::{
    AccessKind, Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, Vm,
    VmConfig,
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

/// Execute a `VectorInput` on a `Vm` driven by the given backend (interpreter or
/// JIT). The engine-agnostic core of every oracle — differential JIT-vs-interp
/// runs both through here (§8, testing.md §8.1).
pub fn run_with_backend(input: &VectorInput, backend: Box<dyn Backend>) -> RunOutcome {
    let size = flat_size(&input.mem_init);
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size },
            consistency: MemConsistency::Fast,
        },
        backend,
    );

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
    let exit = cpu.run(&vm, budget);

    RunOutcome {
        cpu: store_snapshot(&cpu),
        mem: read_back(&vm, &input.mem_init),
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
    CpuSnapshot {
        gpr,
        rip: cpu.reg(Reg::Rip),
        flags: cpu.flags().into(),
        fs_base: cpu.reg(Reg::FsBase),
        gs_base: cpu.reg(Reg::GsBase),
        xmm,
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
        Exit::Breakpoint { addr } => ExitKind::Exception {
            addr: *addr,
            vector: 3,
        },
        Exit::BudgetExhausted => ExitKind::Budget,
        Exit::Fault(kind) => panic!("engine reported an internal fault: {kind:?}"),
    }
}

fn access_kind(a: AccessKind) -> Access {
    match a {
        AccessKind::Read => Access::Read,
        AccessKind::Write => Access::Write,
        AccessKind::Execute => Access::Execute,
    }
}
