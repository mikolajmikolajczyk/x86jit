//! Pipe roundtrip inside a single guest process (OCI-4). Proves the shim's
//! `pipe(2)` end-to-end: create a pipe, `write` the write end, `read` the read
//! end back, and `write` the result to stdout. No fork yet — fork/wait4 land in
//! the next rung; this pins the pipe fd plumbing (PipeBuf, PipeRead/PipeWrite,
//! dup/close counts) on its own. Runs under both engines; the pipe buffer is a
//! host-side data structure, so interpreter and JIT must agree.

use iced_x86::code_asm::*;
use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_linux::Scheduler;
use x86jit_tests::syscall::LinuxShim;

const FLAT_SIZE: u64 = 0x10_0000;
const CODE_BASE: u64 = 0x1000;
const DATA_BASE: u64 = 0x4000;
const FDS: u64 = 0x4000; // int[2] the pipe fd numbers land in
const STATUS: u64 = 0x4010; // wait4 exit-status word
const MSG: u64 = 0x4100; // the payload we push through the pipe
const BUF: u64 = 0x4200; // where we read it back

/// Guest program: `pipe(fds); write(fds[1], "hi\n", 3); read(fds[0], buf, 16);
/// write(1, buf, n); exit(0)`. Addresses are absolute (flat model, all < 4 GiB).
fn program() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    // pipe(&fds)
    a.mov(eax, 22u32).unwrap();
    a.mov(edi, FDS as u32).unwrap();
    a.syscall().unwrap();
    // write(fds[1], MSG, 3)
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, dword_ptr(FDS + 4)).unwrap();
    a.mov(esi, MSG as u32).unwrap();
    a.mov(edx, 3u32).unwrap();
    a.syscall().unwrap();
    // read(fds[0], BUF, 16)
    a.mov(eax, 0u32).unwrap();
    a.mov(edi, dword_ptr(FDS)).unwrap();
    a.mov(esi, BUF as u32).unwrap();
    a.mov(edx, 16u32).unwrap();
    a.syscall().unwrap();
    // write(1, BUF, rax)  — rax holds the byte count just read
    a.mov(edx, eax).unwrap();
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, 1u32).unwrap();
    a.mov(esi, BUF as u32).unwrap();
    a.syscall().unwrap();
    // exit(0)
    a.mov(eax, 60u32).unwrap();
    a.xor(edi, edi).unwrap();
    a.syscall().unwrap();
    a.assemble(CODE_BASE).unwrap()
}

fn run(backend: Box<dyn Backend>) -> (Vec<u8>, Option<i32>) {
    run_code(backend, &program())
}

fn run_code(backend: Box<dyn Backend>, code: &[u8]) -> (Vec<u8>, Option<i32>) {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT_SIZE },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(CODE_BASE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.map(DATA_BASE, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE_BASE, code).unwrap();
    vm.write_bytes(MSG, b"hi\n").unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE_BASE);

    let mut shim = LinuxShim::new();
    for _ in 0..100 {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &mut vm) {
                    break;
                }
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    (shim.stdout, shim.exit_code)
}

/// `fcntl(1, F_DUPFD, 10)` must duplicate stdout to fd 10 and return 10, so a write
/// to the new fd reaches stdout. Before the fix fcntl returned 0 for every command,
/// so the write went to fd 0 (stdin) and was swallowed — empty output.
fn fcntl_dupfd_program() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    // fcntl(1 /*stdout*/, 0 /*F_DUPFD*/, 10)
    a.mov(eax, 72u32).unwrap();
    a.mov(edi, 1u32).unwrap();
    a.mov(esi, 0u32).unwrap();
    a.mov(edx, 10u32).unwrap();
    a.syscall().unwrap();
    // write(rax /*the new fd, must be 10*/, MSG, 3)
    a.mov(edi, eax).unwrap();
    a.mov(eax, 1u32).unwrap();
    a.mov(esi, MSG as u32).unwrap();
    a.mov(edx, 3u32).unwrap();
    a.syscall().unwrap();
    // exit(rax == 10 ? 0 : 1) — encode the returned fd check into the exit code
    a.mov(eax, 60u32).unwrap();
    a.xor(edi, edi).unwrap();
    a.syscall().unwrap();
    a.assemble(CODE_BASE).unwrap()
}

#[test]
fn fcntl_dupfd_duplicates_the_fd() {
    let (interp, _) = run_code(Box::new(InterpreterBackend), &fcntl_dupfd_program());
    let (jit, _) = run_code(Box::new(JitBackend::new()), &fcntl_dupfd_program());
    assert_eq!(interp, b"hi\n", "F_DUPFD'd stdout received the write");
    assert_eq!(jit, interp, "jit and interp agree");
}

#[test]
fn pipe_roundtrip_interp_and_jit_agree() {
    let (interp, ic) = run(Box::new(InterpreterBackend));
    let (jit, jc) = run(Box::new(JitBackend::new()));
    assert_eq!(interp, b"hi\n", "interp: pipe delivered the payload to stdout");
    assert_eq!(jit, interp, "jit and interp agree on pipe output");
    assert_eq!(ic, Some(0));
    assert_eq!(jc, Some(0));
}

/// Guest program exercising fork + pipe inheritance + wait4:
/// ```text
/// pipe(fds);
/// if (fork() == 0) {           // child
///     write(fds[1], "hi\n", 3);
///     exit(7);
/// } else {                     // parent
///     wait4(-1, &status, 0, 0);
///     n = read(fds[0], buf, 16);   // reads what the child wrote (shared pipe)
///     write(1, buf, n);            // -> "hi\n"
///     exit((status >> 8) & 0xff);  // -> 7, proving wait4 status plumbing
/// }
/// ```
fn fork_program() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut child = a.create_label();
    // pipe(&fds)
    a.mov(eax, 22u32).unwrap();
    a.mov(edi, FDS as u32).unwrap();
    a.syscall().unwrap();
    // fork()
    a.mov(eax, 57u32).unwrap();
    a.syscall().unwrap();
    a.test(rax, rax).unwrap();
    a.jz(child).unwrap();
    // ---- parent ----
    // wait4(-1, &status, 0, 0)
    a.mov(eax, 61u32).unwrap();
    a.mov(rdi, -1i64).unwrap();
    a.mov(esi, STATUS as u32).unwrap();
    a.xor(edx, edx).unwrap();
    a.xor(r10d, r10d).unwrap();
    a.syscall().unwrap();
    // read(fds[0], BUF, 16)
    a.mov(eax, 0u32).unwrap();
    a.mov(edi, dword_ptr(FDS)).unwrap();
    a.mov(esi, BUF as u32).unwrap();
    a.mov(edx, 16u32).unwrap();
    a.syscall().unwrap();
    // write(1, BUF, rax)
    a.mov(edx, eax).unwrap();
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, 1u32).unwrap();
    a.mov(esi, BUF as u32).unwrap();
    a.syscall().unwrap();
    // exit((status >> 8) & 0xff)
    a.mov(eax, dword_ptr(STATUS)).unwrap();
    a.shr(eax, 8u32).unwrap();
    a.and(eax, 0xffu32).unwrap();
    a.mov(edi, eax).unwrap();
    a.mov(eax, 60u32).unwrap();
    a.syscall().unwrap();
    // ---- child ----
    a.set_label(&mut child).unwrap();
    // write(fds[1], MSG, 3)
    a.mov(eax, 1u32).unwrap();
    a.mov(edi, dword_ptr(FDS + 4)).unwrap();
    a.mov(esi, MSG as u32).unwrap();
    a.mov(edx, 3u32).unwrap();
    a.syscall().unwrap();
    // exit(7)
    a.mov(eax, 60u32).unwrap();
    a.mov(edi, 7u32).unwrap();
    a.syscall().unwrap();
    a.assemble(CODE_BASE).unwrap()
}

fn run_forking(backend: Box<dyn Backend>, make_backend: impl Fn() -> Box<dyn Backend> + 'static) -> (Vec<u8>, i32) {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT_SIZE },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    vm.map(CODE_BASE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.map(DATA_BASE, 0x1000, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE_BASE, &fork_program()).unwrap();
    vm.write_bytes(MSG, b"hi\n").unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE_BASE);

    let out = Scheduler::new(make_backend)
        .run(vm, cpu, LinuxShim::new())
        .expect("process tree ran");
    (out.stdout, out.exit_code)
}

#[test]
fn fork_pipe_wait_interp() {
    let (stdout, code) = run_forking(Box::new(InterpreterBackend), || Box::new(InterpreterBackend));
    assert_eq!(stdout, b"hi\n", "child wrote the pipe, parent read it after wait4");
    assert_eq!(code, 7, "wait4 delivered the child's exit code");
}

#[test]
fn fork_pipe_wait_jit() {
    let (stdout, code) = run_forking(Box::new(JitBackend::new()), || Box::new(JitBackend::new()));
    assert_eq!(stdout, b"hi\n");
    assert_eq!(code, 7);
}
