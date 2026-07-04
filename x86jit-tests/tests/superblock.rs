//! Superblocks (M5-T3b): a straight-line region of guest blocks joined by
//! unconditional jumps compiles as one JIT function. Verifies (1) the region forms
//! and runs identically to the interpreter, (2) the region counter fires, and (3)
//! the fuel gate charges the exact guest-block count so a `Blocks(n)` run stops at
//! the same block — and in the same state — as the interpreter (the invariant the
//! differential oracle depends on).

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionCaps,
    RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x2_0000;
const CODE: u64 = 0x1000;
const OUT: u64 = 0x8000;

/// Three basic blocks chained by `jmp`: `rax=10; jmp; rax+=20; jmp; rax+=5;
/// [OUT]=rax; hlt`. The two `jmp`s make it a 3-block straight-line region.
fn program() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut l1 = a.create_label();
    let mut l2 = a.create_label();
    a.mov(rax, 10i64).unwrap();
    a.jmp(l1).unwrap();
    a.set_label(&mut l1).unwrap();
    a.add(rax, 20i32).unwrap();
    a.jmp(l2).unwrap();
    a.set_label(&mut l2).unwrap();
    a.add(rax, 5i32).unwrap();
    a.mov(qword_ptr(OUT), rax).unwrap();
    a.hlt().unwrap();
    a.assemble(CODE).unwrap()
}

fn vm_with(backend: Box<dyn Backend>) -> Vm {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
    let code = program();
    for (i, b) in code.iter().enumerate() {
        vm.mem.write(CODE + i as u64, *b as u64, 1).unwrap();
    }
    vm
}

const CAPS: RegionCaps = RegionCaps {
    max_blocks: 16,
    max_icount: 256,
};

/// Run to `hlt`; return (rax, mem[OUT]).
fn run_to_hlt(vm: &Vm, mut cpu: x86jit_core::Vcpu) -> (u64, u64) {
    cpu.set_reg(Reg::Rip, CODE);
    loop {
        match cpu.run(vm, None) {
            Exit::Hlt => break,
            Exit::BudgetExhausted => continue,
            o => panic!("unexpected exit at {:#x}: {o:?}", cpu.reg(Reg::Rip)),
        }
    }
    (cpu.reg(Reg::Rax), vm.mem.read(OUT, 8).unwrap())
}

#[test]
fn straight_line_region_matches_interpreter_and_fires() {
    let ivm = vm_with(Box::new(InterpreterBackend));
    let icpu = ivm.new_vcpu();
    let interp = run_to_hlt(&ivm, icpu);

    let jvm = vm_with(Box::new(JitBackend::with_superblocks(CAPS)));
    let jcpu = jvm.new_vcpu();
    let jit = run_to_hlt(&jvm, jcpu);

    assert_eq!(interp, (35, 35), "reference: rax and [OUT] are both 35");
    assert_eq!(jit, interp, "superblock JIT must match the interpreter");
    assert!(
        jvm.cache.regions() >= 1,
        "a multi-block region should have formed"
    );
}

/// Multi-span SMC (M5-T3b / §10): a store onto a byte of the region's **second**
/// sub-block must invalidate the whole cached region (keyed by its entry), so
/// re-execution re-lifts the modified bytes.
#[test]
fn writing_into_a_regions_second_subblock_invalidates_it() {
    // `jmp l1; l1: mov rax, IMM; [OUT]=rax; hlt` — a 2-block region; `l1` (the
    // second sub-block) starts right after the 2-byte `jmp`.
    fn prog(imm: i32) -> Vec<u8> {
        let mut a = CodeAssembler::new(64).unwrap();
        let mut l1 = a.create_label();
        a.jmp(l1).unwrap();
        a.set_label(&mut l1).unwrap();
        a.mov(rax, imm as i64).unwrap();
        a.mov(qword_ptr(OUT), rax).unwrap();
        a.hlt().unwrap();
        a.assemble(CODE).unwrap()
    }
    let v42 = prog(42);
    let v99 = prog(99);
    assert_eq!(v42.len(), v99.len());
    let l1_off = 2usize; // the `jmp` to the next instruction is 2 bytes

    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        Box::new(JitBackend::with_superblocks(CAPS)),
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
    for (i, b) in v42.iter().enumerate() {
        vm.mem.write(CODE + i as u64, *b as u64, 1).unwrap();
    }
    let first = run_to_hlt(&vm, vm.new_vcpu());
    assert_eq!(first, (42, 42), "first run decodes IMM=42");

    // Overwrite the second sub-block (from l1) with the IMM=99 variant — SMC.
    for (i, b) in v99[l1_off..].iter().enumerate() {
        vm.mem
            .write(CODE + (l1_off + i) as u64, *b as u64, 1)
            .unwrap();
    }
    let second = run_to_hlt(&vm, vm.new_vcpu());
    assert_eq!(
        second,
        (99, 99),
        "region must be re-lifted after the SMC write"
    );
}

/// DAG region (M5-T3c): an if/else diamond that re-joins. Both arms and the merge
/// block compile into one function via internal `brif`/`jump`; the exit `hlt` leaves
/// the region. Verified on both arms against the interpreter.
#[test]
fn diamond_region_matches_interpreter_on_both_arms() {
    // `cmp rbx,5; jne else; rax=100; jmp end; else: rax=200; end: rax+=rcx; [OUT]=rax; hlt`
    fn diamond() -> Vec<u8> {
        let mut a = CodeAssembler::new(64).unwrap();
        let mut l_else = a.create_label();
        let mut l_end = a.create_label();
        a.cmp(rbx, 5i32).unwrap();
        a.jne(l_else).unwrap();
        a.mov(rax, 100i64).unwrap();
        a.jmp(l_end).unwrap();
        a.set_label(&mut l_else).unwrap();
        a.mov(rax, 200i64).unwrap();
        a.set_label(&mut l_end).unwrap();
        a.add(rax, rcx).unwrap();
        a.mov(qword_ptr(OUT), rax).unwrap();
        a.hlt().unwrap();
        a.assemble(CODE).unwrap()
    }

    let run = |backend: Box<dyn Backend>, rbx_val: u64| -> (u64, u64) {
        let mut vm = Vm::with_backend(
            VmConfig {
                memory_model: MemoryModel::Flat { size: FLAT },
                consistency: MemConsistency::Fast,
            },
            backend,
        );
        vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
        for (i, b) in diamond().iter().enumerate() {
            vm.mem.write(CODE + i as u64, *b as u64, 1).unwrap();
        }
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        cpu.set_reg(Reg::Rbx, rbx_val);
        cpu.set_reg(Reg::Rcx, 1);
        loop {
            match cpu.run(&vm, None) {
                Exit::Hlt => break,
                Exit::BudgetExhausted => continue,
                o => panic!("exit at {:#x}: {o:?}", cpu.reg(Reg::Rip)),
            }
        }
        (cpu.reg(Reg::Rax), vm.mem.read(OUT, 8).unwrap())
    };

    for rbx_val in [5u64, 7] {
        let interp = run(Box::new(InterpreterBackend), rbx_val);
        let jit = run(Box::new(JitBackend::with_superblocks(CAPS)), rbx_val);
        assert_eq!(jit, interp, "diamond mismatch for rbx={rbx_val}");
    }
    // rbx=5 takes the `then` arm (100+1); rbx=7 the `else` arm (200+1).
    assert_eq!(
        run(Box::new(JitBackend::with_superblocks(CAPS)), 5),
        (101, 101)
    );
    assert_eq!(
        run(Box::new(JitBackend::with_superblocks(CAPS)), 7),
        (201, 201)
    );
}

/// Loop region (M5-T3d): a guest loop's back-edge is internalized, so the whole
/// loop compiles into one function with a real host loop. It must (1) compute the
/// same result as the interpreter, (2) fire the region counter, and (3) stay
/// preemptible — a `Blocks(n)` budget stops mid-loop at the same iteration, and the
/// same state, as the interpreter (the fuel gate at the loop header, §9.2).
#[test]
fn loop_region_matches_interpreter_and_stays_preemptible() {
    // xor rcx,rcx; jmp top; top: add rcx,1; cmp rcx,1000; jb top; [OUT]=rcx; hlt
    fn loop_prog() -> Vec<u8> {
        let mut a = CodeAssembler::new(64).unwrap();
        let mut top = a.create_label();
        a.xor(rcx, rcx).unwrap();
        a.jmp(top).unwrap();
        a.set_label(&mut top).unwrap();
        a.add(rcx, 1i32).unwrap();
        a.cmp(rcx, 1000i32).unwrap();
        a.jb(top).unwrap();
        a.mov(qword_ptr(OUT), rcx).unwrap();
        a.hlt().unwrap();
        a.assemble(CODE).unwrap()
    }
    // Run with `budget`; return (rcx, hit_hlt, regions_fired).
    let run = |backend: Box<dyn Backend>, budget: Option<u64>| -> (u64, bool, u64) {
        let mut vm = Vm::with_backend(
            VmConfig {
                memory_model: MemoryModel::Flat { size: FLAT },
                consistency: MemConsistency::Fast,
            },
            backend,
        );
        vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
        for (i, b) in loop_prog().iter().enumerate() {
            vm.mem.write(CODE + i as u64, *b as u64, 1).unwrap();
        }
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        // One `run` reaches the terminal event: `hlt` (no budget) or the budget cap.
        let hit_hlt = match cpu.run(&vm, budget) {
            Exit::Hlt => true,
            Exit::BudgetExhausted => false,
            o => panic!("exit at {:#x}: {o:?}", cpu.reg(Reg::Rip)),
        };
        (cpu.reg(Reg::Rcx), hit_hlt, vm.cache.regions())
    };

    // Full run: same result, and the loop formed a region.
    let interp = run(Box::new(InterpreterBackend), None);
    let jit = run(Box::new(JitBackend::with_superblocks(CAPS)), None);
    assert_eq!(jit.0, 1000, "loop counts to 1000");
    assert_eq!(
        (jit.0, jit.1),
        (interp.0, interp.1),
        "loop result must match"
    );
    assert!(jit.2 >= 1, "the loop should compile as a region");

    // Preemption: a 100-block budget stops mid-loop at the same state on both.
    let interp_b = run(Box::new(InterpreterBackend), Some(100));
    let jit_b = run(Box::new(JitBackend::with_superblocks(CAPS)), Some(100));
    assert!(
        !interp_b.1 && interp_b.0 < 1000,
        "budget 100 stops before the loop ends"
    );
    assert_eq!(
        (jit_b.0, jit_b.1),
        (interp_b.0, interp_b.1),
        "a mid-loop budget stop must match the interpreter exactly"
    );
}

/// A `Blocks(n)` run must stop at the same guest block — and same state — under the
/// superblock JIT as under the interpreter (fuel accounting is exact).
#[test]
fn region_fuel_stops_at_the_same_block() {
    // Budget 2 blocks: after block0 (`rax=10; jmp`) and block1 (`rax+=20; jmp`),
    // rax=30 and RIP sits at block2 (`l2`); the third block hasn't run.
    for backend in [
        Box::new(InterpreterBackend) as Box<dyn Backend>,
        Box::new(JitBackend::with_superblocks(CAPS)),
    ] {
        let vm = vm_with(backend);
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        let exit = cpu.run(&vm, Some(2));
        assert!(
            matches!(exit, Exit::BudgetExhausted),
            "should exhaust the 2-block budget"
        );
        assert_eq!(cpu.reg(Reg::Rax), 30, "two blocks ran → rax = 10 + 20");
    }
}
