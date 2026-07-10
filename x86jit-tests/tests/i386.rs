//! i386 (MODE-A) whole-program end-to-end (TASK-197.4): load a static `EM_386`
//! ELF, run it in `CpuMode::Compat32` through the i386 `int 0x80` shim, and assert
//! its observable output three ways — native (an x86-64 host runs an i386 static
//! binary directly), interpreter, JIT.
//!
//! The fixture (`programs/hello_i386.s`) is a freestanding nolibc program: it does
//! `write(1, "hello i386\n", 11)` then `exit(0)` via raw `int 0x80`, so it exercises
//! the loader + lifter + int-0x80 dispatch + i386 syscall numbering without pulling
//! in a libc's TLS/startup churn. The parent MODE-A goal ("a real i386 binary runs
//! 3-way") is met by this static binary; a dynamically-linked i386 userland stays
//! deferred with the rest of C1/C2 (spec §17.6).

use x86jit_core::{
    Backend, CpuMode, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf_i386, setup_stack_i386, LoadError};
use x86jit_linux::shim::LinuxShim;

// The i386 image maps around 0x0804_8000 (the classic i386 ET_EXEC base); give the
// guest a flat space that covers it plus a stack, all below 4 GiB (Compat32).
const FLAT: u64 = 0x0900_0000; // 144 MiB
const STACK_TOP: u64 = 0x08ff_0000;
const STACK_BASE: u64 = 0x08e0_0000;
const HELLO: &[u8] = include_bytes!("../programs/hello_i386.elf");

/// Load `hello_i386.elf` into a Compat32 VM on `backend` and run it to exit through
/// the i386 shim, returning captured stdout and the exit code.
fn run_i386(backend: Box<dyn Backend>) -> (Vec<u8>, Option<i32>) {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
    vm.set_cpu_mode(CpuMode::Compat32);

    let entry = load_static_elf_i386(&mut vm, HELLO).expect("load i386 elf");
    vm.map(
        STACK_BASE,
        (STACK_TOP - STACK_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .expect("map stack");
    let esp =
        setup_stack_i386(&mut vm, STACK_TOP, &[b"hello_i386"], &[]).expect("setup i386 stack");

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, esp);

    let mut shim = LinuxShim::new();
    shim.brk = STACK_BASE - 0x10_0000;
    shim.brk_limit = STACK_BASE;

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
    (shim.stdout, shim.exit_code)
}

/// Native reference: an x86-64 Linux host runs a *static* i386 binary directly
/// (CONFIG_IA32_EMULATION). Tolerate a spawn failure (kernel without ia32) — then
/// the baked expectation stands, and interp/JIT are still checked against it. On a
/// non-x86 host the native leg is skipped entirely.
fn reference() -> Vec<u8> {
    let expected = b"hello i386\n".to_vec();
    #[cfg(target_arch = "x86_64")]
    {
        match std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/hello_i386.elf"
        ))
        .output()
        {
            Ok(out) => {
                assert_eq!(
                    out.stdout, expected,
                    "native i386 output != baked expectation"
                );
                out.stdout
            }
            Err(e) => {
                eprintln!("skipping native i386 leg (no ia32 emulation?): {e}");
                expected
            }
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        expected
    }
}

/// AC#1: the i386 static hello loads and runs to exit under interp and JIT, matching
/// the native reference.
#[test]
fn i386_hello_native_interp_jit_agree() {
    let want = reference();
    let (interp, ic) = run_i386(Box::new(InterpreterBackend));
    let (jit, jc) = run_i386(Box::new(JitBackend::new()));
    assert_eq!(interp, want, "interpreter output != reference");
    assert_eq!(jit, want, "JIT output != reference");
    assert_eq!(ic, Some(0), "interpreter exit code");
    assert_eq!(jc, Some(0), "JIT exit code");
}

/// AC#2: `int 0x80` dispatches through the shim with i386 numbers and 32-bit struct
/// translation. Drive the shim directly with a hand-set i386 register file and assert
/// (a) the i386 `write` number 4 (not the x86-64 1) reaches stdout via EBX/ECX/EDX,
/// and (b) an i386 `writev` (146) reads the 8-byte-per-entry iovec (4-byte pointers),
/// not the x86-64 16-byte layout.
#[test]
fn int80_uses_i386_numbers_and_32bit_structs() {
    let mut vm = Vm::new(VmConfig::flat(0x10_0000));
    vm.set_cpu_mode(CpuMode::Compat32);
    vm.map(0x1000, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
    let mut cpu = vm.new_vcpu();
    let mut shim = LinuxShim::new();

    // (a) write(1, "hi\n", 3) with the i386 numbering: EAX=4, EBX=1, ECX=buf, EDX=3.
    vm.write_bytes(0x1100, b"hi\n").unwrap();
    cpu.set_reg(Reg::Rax, 4); // i386 __NR_write (x86-64 4 == stat — wrong table would misfire)
    cpu.set_reg(Reg::Rbx, 1);
    cpu.set_reg(Reg::Rcx, 0x1100);
    cpu.set_reg(Reg::Rdx, 3);
    assert!(!shim.handle(&mut cpu, &vm));
    assert_eq!(cpu.reg(Reg::Rax), 3, "write returns byte count");
    assert_eq!(shim.stdout, b"hi\n", "i386 write reached stdout");

    // (b) writev(1, iov, 2) — i386 iovec is { u32 base; u32 len } = 8 bytes/entry.
    vm.write_bytes(0x1200, b"AB").unwrap();
    vm.write_bytes(0x1210, b"CDE").unwrap();
    // iov[0] = { base=0x1200, len=2 }, iov[1] = { base=0x1210, len=3 } at 0x1300.
    vm.write_bytes(0x1300, &0x1200u32.to_le_bytes()).unwrap();
    vm.write_bytes(0x1304, &2u32.to_le_bytes()).unwrap();
    vm.write_bytes(0x1308, &0x1210u32.to_le_bytes()).unwrap();
    vm.write_bytes(0x130c, &3u32.to_le_bytes()).unwrap();
    shim.stdout.clear();
    cpu.set_reg(Reg::Rax, 146); // i386 __NR_writev
    cpu.set_reg(Reg::Rbx, 1);
    cpu.set_reg(Reg::Rcx, 0x1300);
    cpu.set_reg(Reg::Rdx, 2);
    assert!(!shim.handle(&mut cpu, &vm));
    assert_eq!(cpu.reg(Reg::Rax), 5, "writev returns total byte count");
    assert_eq!(
        shim.stdout, b"ABCDE",
        "8-byte-per-entry iovec parsed correctly"
    );
}

/// AC#3: non-i386 32-bit (and other unsupported) ELFs are rejected loudly (spec
/// §17.7). The 64-bit x86-64 hello is refused by the i386 loader, and a hand-built
/// big-endian / non-EM_386 32-bit header is refused too — each with a clear message.
#[test]
fn non_i386_elfs_rejected_loudly() {
    let mut vm = Vm::new(VmConfig::flat(0x10_0000));

    // A real 64-bit x86-64 ELF handed to the i386 loader.
    let x64 = include_bytes!("../programs/hello_static.elf");
    match load_static_elf_i386(&mut vm, x64) {
        Err(LoadError::Unsupported(msg)) => {
            assert!(msg.contains("64-bit"), "reason names the mismatch: {msg}");
        }
        other => panic!("expected loud Unsupported rejection, got {other:?}"),
    }

    // A minimal ELF32 header whose e_machine is EM_ARM (40), not EM_386.
    let mut arm32 = vec![0u8; 64];
    arm32[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    arm32[4] = 1; // EI_CLASS = ELFCLASS32
    arm32[5] = 1; // EI_DATA  = ELFDATA2LSB (little-endian)
    arm32[6] = 1; // EI_VERSION
    arm32[16..18].copy_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
    arm32[18..20].copy_from_slice(&40u16.to_le_bytes()); // e_machine = EM_ARM
    match load_static_elf_i386(&mut vm, &arm32) {
        Err(LoadError::Unsupported(msg)) => {
            assert!(
                msg.contains("EM_386"),
                "reason names the machine gap: {msg}"
            );
        }
        other => panic!("expected loud Unsupported rejection, got {other:?}"),
    }

    // And the i386 loader still rejects a truncated / non-ELF blob.
    assert!(matches!(
        load_static_elf_i386(&mut vm, b"not an elf"),
        Err(LoadError::NotElf(_))
    ));
}
