//! task-281: guest instructions executed must be countable in COMPILED code, not
//! only on the interpreter path.
//!
//! `Vcpu::retired_instructions` ticks only in the interpreter — by design, it is a
//! deterministic virtual-time base for a scheduler — so a 64-bit guest running
//! JIT-compiled code reported almost nothing. An embedder (unemups4/Celeste)
//! measured ~23k retired in 10 s against ~1M block transfers per frame, which
//! answers neither "how far from native are we per instruction" nor "how long is the
//! average compiled unit".
//!
//! `Vcpu::executed_instructions` counts both tiers. The interpreter is the oracle:
//! the same program must report the same total whichever tier ran it, because the
//! compiled count is the lifter's own per-block `icount` and must therefore agree.

use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const RAM: u64 = 0x10000;
const ENTRY: u64 = 0x1000;

/// Run `code` to `hlt` and return (executed instructions, retired instructions).
fn run(jit: bool, tier: Option<u32>, code: &[u8], rcx: u64) -> (u64, u64) {
    let backend: Box<dyn Backend> = if jit {
        let b = JitBackend::new();
        b.enable_icount(); // opt-in: off by default, it costs a load/add/store per block
        Box::new(b)
    } else {
        Box::new(InterpreterBackend)
    };
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), backend);
    vm.set_tier_up_after(tier);
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, ENTRY);
    cpu.set_reg(Reg::Rcx, rcx);
    assert!(matches!(cpu.run(&vm, None), Exit::Hlt));
    (cpu.executed_instructions(), cpu.retired_instructions())
}

/// A loop, so most instructions run in a compiled block rather than in the
/// pre-tier-up interpreter warmup.
///   L: dec ecx ; jnz L ; hlt
const LOOP_CODE: &[u8] = &[0xFF, 0xC9, 0x75, 0xFC, 0xF4];

#[test]
fn compiled_code_counts_the_same_instructions_as_the_interpreter() {
    const N: u64 = 5_000;
    let (interp_exec, interp_retired) = run(false, None, LOOP_CODE, N);
    // dec+jnz per iteration, the final non-taken jnz, plus hlt.
    let expect = 2 * N + 1;
    assert_eq!(interp_exec, expect, "interpreter is the oracle");
    assert_eq!(
        interp_retired, expect,
        "on the interpreter both counters agree"
    );

    for tier in [Some(0), Some(1), Some(16)] {
        let (jit_exec, jit_retired) = run(true, tier, LOOP_CODE, N);
        assert_eq!(
            jit_exec, expect,
            "tier_up_after={tier:?}: compiled code must count the same instructions \
             as the interpreter, got {jit_exec} want {expect}"
        );
        // The point of the task: `retired` alone would have missed nearly all of it.
        assert!(
            jit_retired < jit_exec,
            "tier_up_after={tier:?}: retired ({jit_retired}) should lag executed \
             ({jit_exec}) — if they match, nothing was actually compiled and the \
             test is not exercising the compiled path"
        );
    }
}

/// Eager compilation: every block compiled on first execution, so essentially all
/// the work is on the compiled path and `retired` sees almost none of it.
#[test]
fn eager_compilation_is_counted_too() {
    const N: u64 = 2_000;
    let (exec, retired) = run(true, None, LOOP_CODE, N);
    assert_eq!(exec, 2 * N + 1);
    assert!(
        retired * 20 < exec,
        "eager: retired ({retired}) should be a tiny fraction of executed ({exec})"
    );
}

/// The other number the task asks for: `executed / chained` is the average length of
/// a compiled unit. It is only meaningful if both advance on the same run.
#[test]
fn executed_and_chained_together_give_the_average_unit_length() {
    const N: u64 = 5_000;
    let jit = JitBackend::new();
    jit.enable_icount();
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), Box::new(jit));
    vm.set_tier_up_after(Some(1));
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, LOOP_CODE).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, ENTRY);
    cpu.set_reg(Reg::Rcx, N);
    assert!(matches!(cpu.run(&vm, None), Exit::Hlt));

    let (exec, chained) = (cpu.executed_instructions(), vm.cache.chained());
    assert!(chained > 0, "the loop must chain");
    // This block is `dec ecx; jnz L` — two guest instructions per chained transfer.
    let avg = exec as f64 / chained as f64;
    assert!(
        (1.5..=3.0).contains(&avg),
        "average compiled-unit length {avg:.2} should be ~2 for this loop \
         (executed={exec}, chained={chained})"
    );
}

/// Off by default: an embedder that never asks must not pay the per-block accounting,
/// and must not silently read zeros believing they are real. The counter still tracks
/// the interpreter, which needs no codegen.
#[test]
fn accounting_is_opt_in() {
    const N: u64 = 2_000;
    let jit = JitBackend::new();
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), Box::new(jit));
    vm.set_tier_up_after(Some(0)); // compile after the first execution
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, LOOP_CODE).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, ENTRY);
    cpu.set_reg(Reg::Rcx, N);
    assert!(matches!(cpu.run(&vm, None), Exit::Hlt));

    let exec = cpu.executed_instructions();
    assert!(
        exec < 2 * N + 1,
        "without enable_icount the compiled blocks must not be counted; got {exec}"
    );
    assert!(
        vm.backend.codegen_description().contains("icount=false"),
        "the state must be visible: {}",
        vm.backend.codegen_description()
    );
}
