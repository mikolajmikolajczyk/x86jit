//! JIT config-matrix acceptance (M4, testing.md §8.1): the Cranelift backend must
//! produce identical state to the interpreter on every input. The interpreter is
//! the oracle for the JIT (§8).

use iced_x86::code_asm::*;
use x86jit_core::{
    Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_tests::compare::{check, compare};
use x86jit_tests::oracle::{run_with_backend, InterpreterOracle, Oracle, VectorInput};
use x86jit_tests::syscall::LinuxShim;
use x86jit_tests::vector::{CpuSnapshot, FlagName, MemChunk, MemKind, RunSpec, TestVector};

const CODE: u64 = 0x1000;
const SCRATCH: u64 = 0x8000;
const SCRATCH_LEN: usize = 0x1000;

/// Assemble a snippet, run it on the interpreter and the JIT, assert identical
/// final state (undefined flags masked).
fn jit_eq_interp(
    build: impl FnOnce(&mut CodeAssembler),
    init: impl FnOnce(&mut CpuSnapshot),
    dont_care: &[FlagName],
) {
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();

    let mut cpu = CpuSnapshot { rip: CODE, ..Default::default() };
    init(&mut cpu);

    let input = VectorInput {
        cpu_init: cpu,
        mem_init: vec![
            MemChunk { addr: CODE, bytes: code, kind: MemKind::Ram },
            MemChunk { addr: SCRATCH, bytes: vec![0u8; SCRATCH_LEN], kind: MemKind::Ram },
        ],
        entry: CODE,
        run: RunSpec::UntilExit,
    };

    let interp = run_with_backend(&input, Box::new(InterpreterBackend));
    let jit = run_with_backend(&input, Box::new(JitBackend::new()));
    if let Some(d) = compare(&interp, &jit, dont_care) {
        panic!("JIT diverges from interpreter:\n{d}");
    }
}

#[test]
fn mov_and_zero_extend() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0xFFFF_FFFF_FFFF_FFFFu64).unwrap();
            a.mov(eax, 5i32).unwrap();
            a.mov(cx, 0x1234i32).unwrap(); // 16-bit preserves upper
            a.hlt().unwrap();
        },
        |c| c.gpr[1] = 0xAAAA_BBBB_CCCC_DDDD,
        &[],
    );
}

#[test]
fn add_sub_flags() {
    jit_eq_interp(
        |a| {
            a.mov(eax, 0x7FFF_FFFFi32).unwrap();
            a.add(eax, 1i32).unwrap(); // OF, SF
            a.mov(ebx, 0i32).unwrap();
            a.sub(ebx, 1i32).unwrap(); // CF, SF
            a.cmp(eax, eax).unwrap(); // ZF
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn adc_sbb_chain() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0xFFFF_FFFF_FFFF_FFFFu64).unwrap();
            a.add(rax, 1i32).unwrap();
            a.mov(rcx, 5u64).unwrap();
            a.adc(rcx, 0i32).unwrap();
            a.mov(edx, 0i32).unwrap();
            a.sub(edx, 1i32).unwrap();
            a.mov(esi, 10i32).unwrap();
            a.sbb(esi, 3i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn logic_inc_dec_neg_not() {
    jit_eq_interp(
        |a| {
            a.mov(eax, 0xF0F0i32).unwrap();
            a.and(eax, 0x0FF0i32).unwrap();
            a.or(eax, 0x0003i32).unwrap();
            a.xor(eax, 0x00FFi32).unwrap();
            a.inc(eax).unwrap();
            a.dec(eax).unwrap();
            a.neg(eax).unwrap();
            a.not(eax).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af],
    );
}

#[test]
fn extend_and_convert() {
    jit_eq_interp(
        |a| {
            a.mov(ebx, 0x80i32).unwrap();
            a.movzx(eax, bl).unwrap();
            a.movsx(ecx, bl).unwrap();
            a.mov(eax, -3i32).unwrap();
            a.movsxd(rdx, eax).unwrap();
            a.cdqe().unwrap();
            a.mov(rax, 0xFFFF_FFFF_FFFF_FFFFu64).unwrap();
            a.cqo().unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn addressing_load_store() {
    jit_eq_interp(
        |a| {
            a.mov(rbx, 0x10u64).unwrap();
            a.mov(rcx, 0x3u64).unwrap();
            a.lea(rax, qword_ptr(rbx + rcx * 4 + 8)).unwrap();
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rdx, qword_ptr(SCRATCH)).unwrap();
            a.mov(byte_ptr(SCRATCH + 16), 0xABi32).unwrap();
            a.movzx(esi, byte_ptr(SCRATCH + 16)).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn conditional_loop_and_setcc_cmov() {
    jit_eq_interp(
        |a| {
            let mut top = a.create_label();
            a.mov(ecx, 5i32).unwrap();
            a.mov(eax, 0i32).unwrap();
            a.set_label(&mut top).unwrap();
            a.add(eax, ecx).unwrap();
            a.sub(ecx, 1i32).unwrap();
            a.jnz(top).unwrap();
            a.cmp(eax, 10i32).unwrap();
            a.setg(bl).unwrap();
            a.mov(edx, 0x1111i32).unwrap();
            a.mov(esi, 0x2222i32).unwrap();
            a.cmovg(edx, esi).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn push_pop_call_ret() {
    jit_eq_interp(
        |a| {
            let mut func = a.create_label();
            a.mov(rax, 0xDEAD_BEEFu64).unwrap();
            a.push(rax).unwrap();
            a.pop(rbx).unwrap();
            a.call(func).unwrap();
            a.hlt().unwrap();
            a.set_label(&mut func).unwrap();
            a.mov(ecx, 42i32).unwrap();
            a.ret().unwrap();
        },
        |c| c.gpr[4] = SCRATCH + 0x800,
        &[],
    );
}

#[test]
fn store_out_of_bounds_traps_like_interp() {
    // A store to an address past the flat buffer must trap identically.
    jit_eq_interp(
        |a| {
            a.mov(rax, 1u64).unwrap();
            a.mov(qword_ptr(0x9_0000u64), rax).unwrap(); // > flat size (0x9000)
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

// --- corpus replay on the JIT + whole programs ---

#[test]
fn corpus_replays_on_jit() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors");
    let mut files = Vec::new();
    collect_ron(&root, &mut files);
    assert!(!files.is_empty());

    for file in &files {
        let vector = TestVector::from_ron(&std::fs::read_to_string(file).unwrap()).unwrap();
        let input = VectorInput {
            cpu_init: vector.cpu_init.clone(),
            mem_init: vector.mem_init.clone(),
            entry: vector.entry,
            run: vector.run,
        };
        let jit = run_with_backend(&input, Box::new(JitBackend::new()));
        if let Some(d) = check(&vector, &jit) {
            panic!("JIT diverges from vector {}:\n{d}", vector.name);
        }
        // And the JIT agrees with the interpreter directly.
        let interp = InterpreterOracle.run(&input);
        assert!(compare(&interp, &jit, &vector.dont_care_flags).is_none());
    }
}

#[test]
fn hello_runs_on_jit() {
    let (stdout, code) = run_program_on_jit(include_bytes!("../programs/hello_static.elf"), &[b"hello"]);
    assert_eq!(stdout, b"hello\n");
    assert_eq!(code, Some(0));
}

#[test]
fn echo_argv_runs_on_jit() {
    let (stdout, code) =
        run_program_on_jit(include_bytes!("../programs/echo_argv.elf"), &[b"echo_argv", b"WORLD"]);
    assert_eq!(stdout, b"WORLD");
    assert_eq!(code, Some(2));
}

fn run_program_on_jit(image: &[u8], argv: &[&[u8]]) -> (Vec<u8>, Option<i32>) {
    use x86jit_elf::{load_static_elf, setup_stack};

    const FLAT: u64 = 0x50_0000;
    const STACK_BASE: u64 = 0x48_0000;
    const STACK_TOP: u64 = 0x50_0000;

    let mut vm = Vm::with_backend(
        VmConfig { memory_model: MemoryModel::Flat { size: FLAT }, consistency: MemConsistency::Fast },
        Box::new(JitBackend::new()),
    );
    let entry = load_static_elf(&mut vm, image).unwrap();
    vm.map(STACK_BASE, (STACK_TOP - STACK_BASE) as usize, Prot::RW, RegionKind::Ram)
        .unwrap();
    let stack_ptr = setup_stack(&mut vm, STACK_TOP, argv, &[]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, stack_ptr);

    let mut shim = LinuxShim::new();
    for _ in 0..10_000 {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &vm) {
                    break;
                }
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    (shim.stdout, shim.exit_code)
}

/// Measured JIT speedup over the interpreter on a hot arithmetic loop (§12 M4).
/// Ignored by default (timing is machine-dependent); run with `--ignored --nocapture`.
#[test]
#[ignore]
fn jit_speedup() {
    let n = 5_000_000i32;
    let build = |a: &mut CodeAssembler| {
        let mut top = a.create_label();
        a.mov(ecx, n).unwrap();
        a.mov(eax, 0i32).unwrap();
        a.set_label(&mut top).unwrap();
        a.add(eax, ecx).unwrap();
        a.sub(ecx, 1i32).unwrap();
        a.jnz(top).unwrap();
        a.hlt().unwrap();
    };
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();
    let input = VectorInput {
        cpu_init: CpuSnapshot { rip: CODE, ..Default::default() },
        mem_init: vec![MemChunk { addr: CODE, bytes: code, kind: MemKind::Ram }],
        entry: CODE,
        run: RunSpec::Blocks(u64::MAX),
    };

    let t0 = std::time::Instant::now();
    let i = run_with_backend(&input, Box::new(InterpreterBackend));
    let interp_ms = t0.elapsed().as_secs_f64() * 1e3;

    let t1 = std::time::Instant::now();
    let j = run_with_backend(&input, Box::new(JitBackend::new()));
    let jit_ms = t1.elapsed().as_secs_f64() * 1e3;

    assert!(compare(&i, &j, &[]).is_none(), "JIT result must match interpreter");
    eprintln!(
        "loop of {n} iters: interp {interp_ms:.1} ms, jit {jit_ms:.1} ms, speedup {:.1}x",
        interp_ms / jit_ms
    );
    assert!(jit_ms < interp_ms, "JIT should beat the interpreter on a hot loop");
}

fn collect_ron(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_ron(&path, out);
        } else if path.extension().is_some_and(|e| e == "ron") {
            out.push(path);
        }
    }
}
