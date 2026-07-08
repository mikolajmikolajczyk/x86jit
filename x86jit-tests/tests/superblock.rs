//! Superblocks (M5-T3b): a straight-line region of guest blocks joined by
//! unconditional jumps compiles as one JIT function. Verifies (1) the region forms
//! and runs identically to the interpreter, (2) the region counter fires, and (3)
//! the fuel gate charges the exact guest-block count so a `Blocks(n)` run stops at
//! the same block — and in the same state — as the interpreter (the invariant the
//! differential oracle depends on).

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, Prot, Reg, RegionCaps, RegionKind, Vm, VmConfig,
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
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
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

/// A loop-free chain runs identically to the interpreter and — under the T3f
/// policy — stays single-block (only loops are worth a region's heavier compile).
#[test]
fn straight_line_chain_matches_interpreter_and_stays_single_block() {
    let ivm = vm_with(Box::new(InterpreterBackend));
    let icpu = ivm.new_vcpu();
    let interp = run_to_hlt(&ivm, icpu);

    let jvm = vm_with(Box::new(JitBackend::with_superblocks(CAPS)));
    let jcpu = jvm.new_vcpu();
    let jit = run_to_hlt(&jvm, jcpu);

    assert_eq!(interp, (35, 35), "reference: rax and [OUT] are both 35");
    assert_eq!(jit, interp, "superblock JIT must match the interpreter");
    assert_eq!(
        jvm.cache.regions(),
        0,
        "loop-free code must not form a region"
    );
}

/// Multi-span SMC (§10): a store onto a byte of a region's **second** sub-block
/// must invalidate the whole cached region (keyed by its entry), so re-execution
/// re-lifts the modified bytes. Uses a small loop so a region actually forms.
#[test]
fn writing_into_a_regions_second_subblock_invalidates_it() {
    // `xor rcx,rcx; jmp top; top: mov rax,IMM; inc rcx; cmp rcx,3; jb top;
    //  [OUT]=rax; hlt` — the self-loop makes `top` (the 2nd sub-block) a region.
    fn prog(imm: i32) -> Vec<u8> {
        let mut a = CodeAssembler::new(64).unwrap();
        let mut top = a.create_label();
        a.xor(rcx, rcx).unwrap();
        a.jmp(top).unwrap();
        a.set_label(&mut top).unwrap();
        a.mov(rax, imm as i64).unwrap();
        a.add(rcx, 1i32).unwrap();
        a.cmp(rcx, 3i32).unwrap();
        a.jb(top).unwrap();
        a.mov(qword_ptr(OUT), rax).unwrap();
        a.hlt().unwrap();
        a.assemble(CODE).unwrap()
    }
    let v42 = prog(42);
    let v99 = prog(99);
    assert_eq!(v42.len(), v99.len());
    // `top` starts after `xor rcx,rcx` (3 bytes) + `jmp` (2 bytes).
    let l1_off = 5usize;

    let mut vm = Vm::with_backend(
        VmConfig::flat(FLAT),
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

/// DAG merge inside a region (M5-T3c): a loop whose body is an if/else diamond that
/// re-joins. The two arms flow into a shared merge block via internal `jump`/branch
/// — the region-internal merge (two in-region predecessors) — while the back-edge
/// makes it a region. Verified against the interpreter.
#[test]
fn loop_with_diamond_merge_matches_interpreter() {
    // rax=0; rcx=0; jmp top
    // top:  cmp rcx,1; je two;  rax+=100; jmp cont
    // two:  rax+=200
    // cont: inc rcx; cmp rcx,3; jb top
    // [OUT]=rax; hlt   — 3 iters, rcx=0,1,2 → +100,+200,+100 → rax=400.
    fn prog() -> Vec<u8> {
        let mut a = CodeAssembler::new(64).unwrap();
        let (mut top, mut two, mut cont) = (a.create_label(), a.create_label(), a.create_label());
        a.mov(rax, 0i64).unwrap();
        a.mov(rcx, 0i64).unwrap();
        a.jmp(top).unwrap();
        a.set_label(&mut top).unwrap();
        a.cmp(rcx, 1i32).unwrap();
        a.je(two).unwrap();
        a.add(rax, 100i32).unwrap();
        a.jmp(cont).unwrap();
        a.set_label(&mut two).unwrap();
        a.add(rax, 200i32).unwrap();
        a.set_label(&mut cont).unwrap();
        a.add(rcx, 1i32).unwrap();
        a.cmp(rcx, 3i32).unwrap();
        a.jb(top).unwrap();
        a.mov(qword_ptr(OUT), rax).unwrap();
        a.hlt().unwrap();
        a.assemble(CODE).unwrap()
    }

    let run = |backend: Box<dyn Backend>| -> (u64, u64) {
        let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
        vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
        for (i, b) in prog().iter().enumerate() {
            vm.mem.write(CODE + i as u64, *b as u64, 1).unwrap();
        }
        (run_to_hlt(&vm, vm.new_vcpu()).0, vm.cache.regions())
    };

    let interp = run(Box::new(InterpreterBackend));
    let jit = run(Box::new(JitBackend::with_superblocks(CAPS)));
    assert_eq!(interp.0, 400, "3 iters: +100 +200 +100");
    assert_eq!(jit.0, interp.0, "loop+diamond must match the interpreter");
    assert!(
        jit.1 >= 1,
        "the loop should form a region (with an internal merge)"
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
        let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
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
