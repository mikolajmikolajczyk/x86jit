//! Whole-program end-to-end (M2, testing.md §11): load a freestanding static ELF
//! that issues `write`/`exit` via raw `syscall`, run it under the interpreter
//! through the syscall shim, and assert its observable output — the psychological
//! milestone. This exercises the full pipeline: loader → lift → dispatcher →
//! interpreter → syscall shim.
//!
//! The fixture (`programs/hello_static.s`) is nolibc on purpose (§16): a static
//! glibc binary would run SSE2 `memcpy`/`strlen` in `__libc_start_main` before
//! printing anything, secretly requiring SIMD (M8).

use x86jit_core::{Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::syscall::LinuxShim;

const FLAT_SIZE: u64 = 0x50_0000; // covers the 0x400000-based image + stack
const STACK_BASE: u64 = 0x48_0000;
const STACK_TOP: u64 = 0x50_0000;

#[test]
fn hello_static_elf_prints_hello() {
    let image = include_bytes!("../programs/hello_static.elf");

    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: FLAT_SIZE },
        consistency: MemConsistency::Fast,
    });
    let entry = load_static_elf(&mut vm, image).expect("load static elf");
    vm.map(
        STACK_BASE,
        (STACK_TOP - STACK_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();

    // Full System V initial stack (argc/argv/envp/auxv). This freestanding binary
    // ignores it, but the setup exercises the path real `_start`s depend on.
    let rsp = setup_stack(
        &mut vm,
        STACK_TOP,
        &[b"hello_static.elf"],
        &[b"PATH=/usr/bin"],
    )
    .unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    for _ in 0..100 {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &vm) {
                    break;
                }
            }
            other => panic!("unexpected exit before program finished: {other:?}"),
        }
    }

    assert_eq!(shim.stdout, b"hello\n", "emulated program's stdout");
    assert_eq!(shim.exit_code, Some(0), "exit code");
}

/// Proves `setup_stack` semantically: this ELF reads `argv[1]` off the stack,
/// `write`s it, and `exit`s with `argc`. If the SysV layout were wrong the guest
/// would print garbage / crash rather than echo the argument.
#[test]
fn argv_is_read_from_the_stack() {
    let image = include_bytes!("../programs/echo_argv.elf");

    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: FLAT_SIZE },
        consistency: MemConsistency::Fast,
    });
    let entry = load_static_elf(&mut vm, image).expect("load static elf");
    vm.map(
        STACK_BASE,
        (STACK_TOP - STACK_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();

    // argc = 2 (prog name + "WORLD").
    let rsp = setup_stack(&mut vm, STACK_TOP, &[b"echo_argv", b"WORLD"], &[]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    for _ in 0..1000 {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &vm) {
                    break;
                }
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }

    assert_eq!(shim.stdout, b"WORLD", "guest echoed argv[1] from the stack");
    assert_eq!(shim.exit_code, Some(2), "guest exited with argc");
}
