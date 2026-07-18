//! End-to-end interpreter tests: assemble a small guest program, lift it, run it
//! through `Vcpu::run`, and check the resulting register / flag / memory state.
//! The differential Unicorn oracle (M1 harness) lands later; these hand-checked
//! vectors exercise the lift + interpreter vertical directly.

use iced_x86::code_asm::*;
use x86jit_core::{Exit, Prot, Reg, RegionKind, Vcpu, Vm, VmConfig};

const CODE_BASE: u64 = 0x1000;
const DATA_BASE: u64 = 0x2000;
const STACK_TOP: u64 = 0x4000;

/// Fresh Vm: 64 KiB flat space, RX code region at `CODE_BASE`, RW data+stack above.
fn build_vm() -> Vm {
    let mut vm = Vm::new(VmConfig::flat(0x1_0000));
    vm.map(CODE_BASE, 0x1000, Prot::RX, RegionKind::Ram)
        .unwrap();
    vm.map(DATA_BASE, 0x3000, Prot::RW, RegionKind::Ram)
        .unwrap();
    vm
}

/// Assemble a program at `CODE_BASE`, load it, and run to completion.
fn run(build: impl FnOnce(&mut CodeAssembler), setup: impl FnOnce(&mut Vcpu)) -> (Vcpu, Exit) {
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let bytes = asm.assemble(CODE_BASE).unwrap();

    let vm = build_vm();
    vm.write_bytes(CODE_BASE, &bytes).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE_BASE);
    setup(&mut cpu);
    let exit = cpu.run(&vm, Some(10_000));
    (cpu, exit)
}

#[test]
fn arithmetic_and_upper32_zeroing() {
    let (cpu, exit) = run(
        |a| {
            a.mov(rax, 0x1111_2222_3333_4444u64).unwrap();
            a.mov(eax, 5i32).unwrap(); // 32-bit write zeroes the upper 32 bits
            a.mov(ecx, 3i32).unwrap();
            a.add(eax, ecx).unwrap(); // eax = 8
            a.sub(eax, 1i32).unwrap(); // eax = 7
            a.hlt().unwrap();
        },
        |_| {},
    );
    assert!(matches!(exit, Exit::Hlt));
    assert_eq!(
        cpu.reg(Reg::Rax),
        7,
        "upper 32 bits must be cleared by the mov eax"
    );
    assert_eq!(cpu.reg(Reg::Rcx), 3);
}

#[test]
fn flags_from_sub_zero() {
    let (cpu, _) = run(
        |a| {
            a.mov(eax, 5i32).unwrap();
            a.sub(eax, 5i32).unwrap(); // result 0 -> ZF set, CF/SF/OF clear
            a.hlt().unwrap();
        },
        |_| {},
    );
    let f = cpu.flags();
    assert!(f.zf, "5 - 5 = 0 sets ZF");
    assert!(!f.cf);
    assert!(!f.sf);
    assert!(!f.of);
}

#[test]
fn store_then_load_roundtrip() {
    let (cpu, exit) = run(
        |a| {
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.mov(qword_ptr(DATA_BASE), rax).unwrap();
            a.mov(rbx, qword_ptr(DATA_BASE)).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
    );
    assert!(matches!(exit, Exit::Hlt));
    assert_eq!(cpu.reg(Reg::Rbx), 0x1122_3344_5566_7788);
}

#[test]
fn conditional_loop_counts_down() {
    // ecx=3; eax=0; loop: add eax,ecx; sub ecx,1; jnz loop; hlt
    // eax = 3 + 2 + 1 = 6, ecx = 0. Exercises jcc, block re-lift/cache, flags.
    let (cpu, exit) = run(
        |a| {
            let mut top = a.create_label();
            a.mov(ecx, 3i32).unwrap();
            a.mov(eax, 0i32).unwrap();
            a.set_label(&mut top).unwrap();
            a.add(eax, ecx).unwrap();
            a.sub(ecx, 1i32).unwrap();
            a.jnz(top).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
    );
    assert!(matches!(exit, Exit::Hlt));
    assert_eq!(cpu.reg(Reg::Rax), 6);
    assert_eq!(cpu.reg(Reg::Rcx), 0);
}

#[test]
fn call_and_ret_use_the_stack() {
    // call func; hlt; func: mov eax,42; ret
    let (cpu, exit) = run(
        |a| {
            let mut func = a.create_label();
            a.call(func).unwrap();
            a.hlt().unwrap();
            a.set_label(&mut func).unwrap();
            a.mov(eax, 42i32).unwrap();
            a.ret().unwrap();
        },
        |cpu| cpu.set_reg(Reg::Rsp, STACK_TOP),
    );
    assert!(matches!(exit, Exit::Hlt));
    assert_eq!(cpu.reg(Reg::Rax), 42);
    assert_eq!(
        cpu.reg(Reg::Rsp),
        STACK_TOP,
        "ret must unwind the pushed return address"
    );
}

#[test]
fn push_pop_roundtrip() {
    let (cpu, exit) = run(
        |a| {
            a.mov(rax, 0xDEAD_BEEF_CAFE_B0BAu64).unwrap();
            a.push(rax).unwrap();
            a.pop(rbx).unwrap();
            a.hlt().unwrap();
        },
        |cpu| cpu.set_reg(Reg::Rsp, STACK_TOP),
    );
    assert!(matches!(exit, Exit::Hlt));
    assert_eq!(cpu.reg(Reg::Rbx), 0xDEAD_BEEF_CAFE_B0BA);
    assert_eq!(cpu.reg(Reg::Rsp), STACK_TOP);
}

#[test]
fn fwait_is_a_noop_and_advances_rip() {
    // 0x9B (FWAIT/WAIT) is an x87 sync barrier the Orbis CRT emits as padding
    // (task-194); the interpreter must treat it as a single-byte no-op.
    let (cpu, exit) = run(
        |a| {
            a.mov(eax, 7i32).unwrap(); // 5 bytes
            a.wait().unwrap(); // 0x9B, 1 byte
            a.inc(eax).unwrap(); // proves execution continued past FWAIT
            a.hlt().unwrap();
        },
        |_| {},
    );
    assert!(matches!(exit, Exit::Hlt));
    assert_eq!(cpu.reg(Reg::Rax), 8);
}

#[test]
fn syscall_exits_past_the_instruction() {
    let (cpu, exit) = run(
        |a| {
            a.mov(eax, 60i32).unwrap(); // exit syscall number, just data here
            a.syscall().unwrap();
        },
        |_| {},
    );
    assert!(matches!(exit, Exit::Syscall));
    // RIP points PAST the syscall (mov=5 bytes, syscall=2 bytes).
    assert_eq!(cpu.reg(Reg::Rip), CODE_BASE + 7);
    assert_eq!(cpu.reg(Reg::Rax), 60);
}

#[test]
fn store_to_unmapped_traps_on_the_faulting_instruction() {
    let unmapped = 0x9000u64;
    let (cpu, exit) = run(
        |a| {
            a.mov(rax, 1u64).unwrap(); // 0x1000, len 10 -> next 0x100a
            a.mov(qword_ptr(unmapped), rax).unwrap(); // faults
            a.hlt().unwrap();
        },
        |_| {},
    );
    match exit {
        Exit::UnmappedMemory { addr, .. } => assert_eq!(addr, unmapped),
        other => panic!("expected UnmappedMemory, got {other:?}"),
    }
    // RIP is left on the faulting store, not advanced past it.
    assert_eq!(cpu.reg(Reg::Rip), CODE_BASE + 10);
}

#[test]
fn fninit_resets_the_x87_unit() {
    // `fninit` (DB E3) reinitializes the FPU: control word 0x037F, status word 0
    // (TOP 0). It previously surfaced as `Exit::UnknownInstruction`; reaching `Hlt`
    // proves it now lifts on the shared x87 path (this runs in Long64, the PS4 mode
    // that used to #UD it). `fnclex` is exercised too — it must lift as a no-op.
    let (cpu, exit) = run(
        |a| {
            // Perturb the FPU so the reset is observable: load a non-default control
            // word and push two values so TOP is non-zero.
            a.mov(cx, 0x1234u32).unwrap();
            a.mov(word_ptr(DATA_BASE), cx).unwrap();
            a.fldcw(word_ptr(DATA_BASE)).unwrap();
            a.fld1().unwrap(); // push 1.0 -> TOP = 7
            a.fld1().unwrap(); // push 1.0 -> TOP = 6
            a.fninit().unwrap(); // reset -> CW = 0x037F, TOP = 0
            a.fnclex().unwrap(); // must also lift (no-op: exception flags unmodeled)
            a.fnstcw(word_ptr(DATA_BASE + 8)).unwrap();
            a.mov(cx, word_ptr(DATA_BASE + 8)).unwrap(); // cx = reset control word
            a.fnstsw(ax).unwrap(); // ax = status word (TOP in bits 11-13)
            a.hlt().unwrap();
        },
        |_| {},
    );
    assert!(matches!(exit, Exit::Hlt), "fninit must lift, got {exit:?}");
    assert_eq!(
        cpu.reg(Reg::Rcx) & 0xFFFF,
        0x037F,
        "fninit resets the control word to 0x037F"
    );
    assert_eq!(
        cpu.reg(Reg::Rax) & 0xFFFF,
        0,
        "fninit resets TOP to 0, so the status word reads 0"
    );
}
