//! Translation-cache acceptance (M3, testing.md §11): a guest loop must lift each
//! distinct block once and serve every repeat from the cache. Counters make the
//! "did it actually cache?" question answerable (otherwise a broken cache that
//! silently re-lifts still produces correct results).

use iced_x86::code_asm::*;
use x86jit_core::{Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig};

const CODE: u64 = 0x1000;

/// `ecx = n; top: sub ecx,1; jnz top; hlt`. Three distinct blocks:
/// entry (mov+first sub+jnz), the loop top (sub+jnz), and the hlt.
fn run_countdown(n: i32) -> (u64, u64) {
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    asm.mov(ecx, n).unwrap();
    asm.set_label(&mut top).unwrap();
    asm.sub(ecx, 1).unwrap();
    asm.jnz(top).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: 0x2000 },
        consistency: MemConsistency::Fast,
    });
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(&vm, Some(100_000)), Exit::Hlt));

    (vm.cache.hits(), vm.cache.misses())
}

#[test]
fn loop_body_lifts_once_then_hits_cache() {
    let n = 100;
    let (hits, misses) = run_countdown(n);

    // Three distinct block addresses lifted exactly once each.
    assert_eq!(misses, 3, "each distinct block is lifted once");
    // The loop-top block re-dispatches every iteration, all from the cache.
    assert_eq!(hits, n as u64 - 2, "loop body re-executes from cache, never re-lifts");
}

#[test]
fn hits_scale_with_iterations_misses_do_not() {
    let (h100, m100) = run_countdown(100);
    let (h200, m200) = run_countdown(200);

    assert_eq!(m100, 3);
    assert_eq!(m200, 3, "more iterations must not lift more blocks");
    assert_eq!(h200 - h100, 100, "cache hits grow one-for-one with iterations");
}
