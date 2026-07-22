//! task-282: count calls out of compiled code into the Rust helpers.
//!
//! A helper call is a C-ABI exit from JIT'd code that runs a whole interpreter
//! operation — tens to hundreds of host cycles, against the 1-3 a natively lowered
//! instruction costs. A guest whose hot code hits helpers therefore pays a large
//! per-instruction premium that no mid-end or dispatch tuning can recover, which is
//! the shape of the gap reported from Celeste (~34 cycles per guest instruction while
//! this engine reaches 1.9 on a natively-lowered scalar loop).
//!
//! These tests pin that the counter attributes calls to the right helper and stays at
//! zero for code that needs none — otherwise a reading of "no helpers" could mean
//! either "none were called" or "the counter is not wired".

use x86jit_core::{Exit, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const RAM: u64 = 0x10000;
const ENTRY: u64 = 0x1000;

/// Compile and run `code` to `hlt`, returning the backend's helper-call tally.
fn helper_calls(code: &[u8], seed: &[(Reg, u64)]) -> Vec<(&'static str, u64)> {
    let jit = JitBackend::new();
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), Box::new(jit));
    vm.set_tier_up_after(Some(0)); // compile after the first execution
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, code).unwrap();

    // Twice: the first run compiles, the second executes the compiled block, which is
    // the only path that can reach a helper.
    for _ in 0..2 {
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, ENTRY);
        for &(r, v) in seed {
            cpu.set_reg(r, v);
        }
        assert!(matches!(cpu.run(&vm, None), Exit::Hlt));
    }
    vm.backend.helper_calls()
}

/// Plain scalar arithmetic lowers natively — no helper should fire. If this ever
/// reports calls, the counter is over-counting and every other reading is suspect.
#[test]
fn natively_lowered_code_calls_no_helpers() {
    // add eax, ebx ; sub eax, ecx ; xor edx, edx ; hlt
    let code = [0x01, 0xD8, 0x29, 0xC8, 0x31, 0xD2, 0xF4];
    let calls = helper_calls(&code, &[(Reg::Rax, 7), (Reg::Rbx, 5), (Reg::Rcx, 2)]);
    assert!(
        calls.is_empty(),
        "scalar ALU must lower natively, got helper calls {calls:?}"
    );
}

/// `cpuid` is a helper by design (it is neither hot nor lowerable), so it is a clean
/// positive control: the counter must attribute it, by name.
#[test]
fn a_helper_backed_instruction_is_attributed_by_name() {
    // xor eax, eax ; cpuid ; hlt
    let code = [0x31, 0xC0, 0x0F, 0xA2, 0xF4];
    let calls = helper_calls(&code, &[]);
    assert!(
        calls
            .iter()
            .any(|(name, n)| name.contains("cpuid") && *n > 0),
        "cpuid must be attributed to its helper, got {calls:?}"
    );
}

/// Division is helper-backed too, and running it in a loop shows the counter scales
/// with execution rather than with the number of compiled sites.
#[test]
fn counts_scale_with_executions_not_with_compiled_sites() {
    // L: xor edx, edx ; div ebx ; dec ecx ; jnz L ; hlt   (eax/ebx stay constant)
    let code = [
        0x31, 0xD2, // xor edx, edx
        0xF7, 0xF3, // div ebx
        0xFF, 0xC9, // dec ecx
        0x75, 0xF8, // jnz L
        0xF4, // hlt
    ];
    let seed = [(Reg::Rax, 1000u64), (Reg::Rbx, 7), (Reg::Rcx, 50)];
    let calls = helper_calls(&code, &seed);
    let div = calls
        .iter()
        .find(|(name, _)| name.contains("div"))
        .unwrap_or_else(|| panic!("div helper not attributed, got {calls:?}"));
    // 50 iterations per run; the compiled run is the second one, and the first
    // (interpreted) run reaches no helper from compiled code.
    assert!(
        div.1 >= 50,
        "expected at least one div helper call per iteration, got {}",
        div.1
    );
}
