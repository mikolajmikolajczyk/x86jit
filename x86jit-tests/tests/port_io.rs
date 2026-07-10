//! Port I/O trap-out (task-198): `in`/`out` surface as `Exit::PortIo`, the machine
//! counterpart of MMIO. A scripted embedder answers `in` reads by writing the
//! accumulator (`complete_port_in`, sub-register semantics) and observes `out`
//! writes, then re-enters. Pinned under BOTH backends — the JIT defers port I/O to
//! the interpreter, so interp and JIT must agree on port/size/direction/value.
//!
//! `ins`/`outs` (string port I/O, incl. `rep`) are deliberately NOT lifted: no
//! consumer exists and a correct per-element trap-out needs its own restartable
//! loop. They surface as `Exit::UnknownInstruction`, pinned below (AC#1).

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, PortDir, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const CODE: u64 = 0x1000;

fn backend(jit: bool) -> Box<dyn Backend> {
    if jit {
        Box::new(JitBackend::new())
    } else {
        Box::new(InterpreterBackend)
    }
}

/// One serviced port-I/O event, as the embedder saw it.
#[derive(Debug, PartialEq, Eq)]
struct Event {
    port: u16,
    size: u8,
    out: bool,
    value: u64,
}

/// Run `code` under `jit`, servicing every `Exit::PortIo` until `hlt`. `in` reads
/// are answered from `reads` in order (low `size` bytes used by the guest); `out`
/// writes are recorded. Returns the ordered event log and the final vcpu.
fn drive(code: &[u8], jit: bool, mut reads: Vec<u64>) -> (Vec<Event>, x86jit_core::Vcpu) {
    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), backend(jit));
    vm.map(CODE, 0x1000, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);

    let mut log = Vec::new();
    let mut read_iter = reads.drain(..);
    loop {
        match cpu.run(&vm, Some(1000)) {
            Exit::PortIo {
                port,
                size,
                dir,
                value,
            } => {
                let out = dir == PortDir::Out;
                log.push(Event {
                    port,
                    size,
                    out,
                    value,
                });
                if !out {
                    let v = read_iter.next().expect("scripted read available");
                    cpu.complete_port_in(v);
                }
            }
            Exit::Hlt => break,
            other => panic!("unexpected exit (jit={jit}): {other:?}"),
        }
    }
    (log, cpu)
}

/// `out` carries the accumulator value out; both port encodings and all three
/// widths, under both backends.
#[test]
fn out_traps_with_port_size_and_value() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(eax, 0x1234_5678u32 as i32).unwrap();
    asm.out(0x60i32, al).unwrap(); // out imm8, al   — 1 byte
    asm.out(0x61i32, ax).unwrap(); // out imm8, ax   — 2 bytes
    asm.mov(edx, 0x03f8i32).unwrap();
    asm.out(dx, eax).unwrap(); // out dx, eax    — 4 bytes, dx form
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    for jit in [false, true] {
        let (log, _cpu) = drive(&code, jit, vec![]);
        assert_eq!(
            log,
            vec![
                Event {
                    port: 0x60,
                    size: 1,
                    out: true,
                    value: 0x78
                },
                Event {
                    port: 0x61,
                    size: 2,
                    out: true,
                    value: 0x5678
                },
                Event {
                    port: 0x03f8,
                    size: 4,
                    out: true,
                    value: 0x1234_5678
                },
            ],
            "out events (jit={jit})"
        );
    }
}

/// `in` resumes with the embedder-supplied value merged into `al`/`ax`/`eax` with
/// x86 sub-register semantics: 8/16-bit merge into RAX's upper bits, 32-bit zeroes
/// bits 63:32.
#[test]
fn in_resumes_with_embedder_value_and_subreg_semantics() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(rax, 0xdead_beef_dead_beefu64).unwrap();
    asm.in_(al, 0x60i32).unwrap(); // in al, imm8  — merge low byte
    asm.in_(ax, 0x61i32).unwrap(); // in ax, imm8  — merge low word
    asm.mov(edx, 0x03f8i32).unwrap();
    asm.in_(eax, dx).unwrap(); // in eax, dx   — zero upper 32
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    for jit in [false, true] {
        // The final `in eax` zero-extends, so only its value survives in RAX.
        let (log, cpu) = drive(&code, jit, vec![0xAA, 0xBBCC, 0x1122_3344]);
        assert_eq!(
            log,
            vec![
                Event {
                    port: 0x60,
                    size: 1,
                    out: false,
                    value: 0
                },
                Event {
                    port: 0x61,
                    size: 2,
                    out: false,
                    value: 0
                },
                Event {
                    port: 0x03f8,
                    size: 4,
                    out: false,
                    value: 0
                },
            ],
            "in events (jit={jit})"
        );
        assert_eq!(
            cpu.reg(Reg::Rax),
            0x1122_3344,
            "in eax zero-extends RAX (jit={jit})"
        );
    }
}

/// A read/write round trip against a single modelled port register — the
/// mmio_device-style end-to-end shape (AC#3): write a command, read a status back.
#[test]
fn port_register_round_trip() {
    const PORT: i32 = 0x0cf8;
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(edx, PORT).unwrap();
    asm.mov(eax, 0x0000_0042u32 as i32).unwrap();
    asm.out(dx, eax).unwrap(); // write command 0x42 to the port
    asm.in_(eax, dx).unwrap(); // read the status word back
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    for jit in [false, true] {
        // A tiny device: writing 0x42 arms it; the next read returns 0x1000_0042.
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), backend(jit));
        vm.map(CODE, 0x1000, Prot::RWX, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);

        let mut device: u32 = 0;
        loop {
            match cpu.run(&vm, Some(1000)) {
                Exit::PortIo {
                    port,
                    dir: PortDir::Out,
                    value,
                    ..
                } => {
                    assert_eq!(port, PORT as u16);
                    device = value as u32; // latch the command
                }
                Exit::PortIo {
                    port,
                    dir: PortDir::In,
                    ..
                } => {
                    assert_eq!(port, PORT as u16);
                    cpu.complete_port_in(0x1000_0000 | device as u64);
                }
                Exit::Hlt => break,
                other => panic!("unexpected exit (jit={jit}): {other:?}"),
            }
        }
        assert_eq!(
            cpu.reg(Reg::Rax),
            0x1000_0042,
            "guest observed the device status (jit={jit})"
        );
    }
}

/// AC#1: `ins`/`outs` (+`rep`) are documented-rejected — they lift to
/// `Exit::UnknownInstruction`, never to a `PortIo`, under both backends.
#[test]
fn ins_outs_are_rejected() {
    // Encoded directly: iced's `insb`/`outsw` code_asm helpers return `&mut Self`
    // for the `rep` prefix, which complicates a table of builders. The opcodes are
    // stable and short.
    let cases: &[(&str, &[u8])] = &[
        ("insb", &[0x6c]),
        ("insw", &[0x66, 0x6d]),
        ("insd", &[0x6d]),
        ("outsb", &[0x6e]),
        ("outsw", &[0x66, 0x6f]),
        ("outsd", &[0x6f]),
        ("rep outsw", &[0xf3, 0x66, 0x6f]),
    ];
    for (name, bytes) in cases {
        let mut code = bytes.to_vec();
        code.push(0xf4); // hlt

        for jit in [false, true] {
            let mut vm = Vm::with_backend(VmConfig::flat(0x2000), backend(jit));
            vm.map(CODE, 0x1000, Prot::RWX, RegionKind::Ram).unwrap();
            vm.write_bytes(CODE, &code).unwrap();
            let mut cpu = vm.new_vcpu();
            cpu.set_reg(Reg::Rip, CODE);
            match cpu.run(&vm, Some(1000)) {
                Exit::UnknownInstruction { addr, .. } => {
                    assert_eq!(addr, CODE, "{name} rejected at its address (jit={jit})")
                }
                other => panic!("{name} should be rejected (jit={jit}), got {other:?}"),
            }
        }
    }
}
