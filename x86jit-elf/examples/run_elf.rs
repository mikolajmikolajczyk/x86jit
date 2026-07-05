//! Load a static ELF and run it. `x86jit-elf` parses the program headers into a
//! guest memory map and hands back the entry point (spec §4.2); the core executes
//! it. Syscalls trap out as `Exit::Syscall` for the embedder to service — this
//! example implements only `write` and `exit`, enough for a freestanding program.
//! For a real Linux userland (a full syscall shim, fd table, fork) use
//! `x86jit-linux`.
//!
//! Build a suitable input and run it:
//! ```sh
//! printf '.globl _start\n_start:\n mov $1,%%eax; mov $1,%%edi;\n \
//!   lea msg(%%rip),%%rsi; mov $6,%%edx; syscall\n \
//!   mov $60,%%eax; xor %%edi,%%edi; syscall\n \
//!   msg: .ascii "hello\\n"\n' > hello.s
//! cc -static -nostdlib -o hello hello.s
//! cargo run -p x86jit-elf --example run_elf -- hello
//! ```

use std::io::Write;
use x86jit_core::{Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_elf::{load_static_elf, setup_stack};

const FLAT: u64 = 0x100_0000; // 16 MiB guest space
const STACK_TOP: u64 = 0x100_0000;
const STACK_SIZE: u64 = 0x10_0000; // 1 MiB stack

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: run_elf <static-elf>");
        std::process::exit(2);
    });
    let image = std::fs::read(&path).expect("read ELF file");

    let mut vm = Vm::new(VmConfig {
        memory_model: MemoryModel::Flat { size: FLAT },
        consistency: MemConsistency::Fast,
    });
    let entry = load_static_elf(&mut vm, &image).expect("load static ELF");

    let stack_base = STACK_TOP - STACK_SIZE;
    vm.map(stack_base, STACK_SIZE as usize, Prot::RW, RegionKind::Ram)
        .unwrap();
    let arg = path.clone();
    let sp = setup_stack(&mut vm, STACK_TOP, &[arg.as_bytes()], &[]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, sp);

    // Service syscalls until the guest exits. Bounded so a misbehaving guest can't
    // spin the host forever.
    for _ in 0..1_000_000 {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                let nr = cpu.reg(Reg::Rax);
                match nr {
                    // write(fd, buf, len)
                    1 => {
                        let (fd, buf, len) =
                            (cpu.reg(Reg::Rdi), cpu.reg(Reg::Rsi), cpu.reg(Reg::Rdx));
                        let mut bytes = vec![0u8; len as usize];
                        vm.read_bytes(buf, &mut bytes).unwrap();
                        if fd == 2 {
                            std::io::stderr().write_all(&bytes).unwrap();
                        } else {
                            std::io::stdout().write_all(&bytes).unwrap();
                        }
                        cpu.set_reg(Reg::Rax, len); // bytes written
                    }
                    // exit / exit_group
                    60 | 231 => {
                        let code = cpu.reg(Reg::Rdi) as i32;
                        std::io::stdout().flush().ok();
                        eprintln!("[guest exited: {code}]");
                        std::process::exit(code);
                    }
                    other => {
                        eprintln!(
                            "[unsupported syscall {other}; this example implements only \
                             write+exit — use x86jit-linux for a full shim]"
                        );
                        std::process::exit(1);
                    }
                }
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    eprintln!("[syscall budget exhausted]");
}
