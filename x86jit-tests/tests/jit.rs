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
fn shifts_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(eax, 0x8001_0003u32 as i32).unwrap();
            a.shl(eax, 1i32).unwrap();
            a.shr(eax, 3i32).unwrap();
            a.sar(eax, 2i32).unwrap();
            a.mov(rbx, 0xFF00_0000_0000_00F0u64).unwrap();
            a.sar(rbx, 4i32).unwrap();
            a.mov(ecx, 5i32).unwrap();
            a.mov(edx, 0x1234i32).unwrap();
            a.shl(edx, cl).unwrap(); // shift by CL
            a.shr(edx, 0i32).unwrap(); // count 0: flags unchanged
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn rotates_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(eax, 0x8000_0001u32 as i32).unwrap();
            a.rol(eax, 1i32).unwrap();
            a.ror(eax, 3i32).unwrap();
            a.mov(rbx, 0x1234_5678_9ABC_DEF0u64).unwrap();
            a.rol(rbx, 13i32).unwrap();
            a.mov(cl, 5i32).unwrap();
            a.mov(edx, 0x00FF_00FFi32).unwrap();
            a.ror(edx, cl).unwrap();
            a.mov(si, 0x1234i32).unwrap();
            a.rol(si, 4i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn mul_imul_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(eax, 0x0012_3456i32).unwrap();
            a.mov(ebx, 0x0000_789Ai32).unwrap();
            a.mul(ebx).unwrap(); // one-op unsigned -> edx:eax
            a.mov(eax, -100_000i32).unwrap();
            a.mov(ecx, 7i32).unwrap();
            a.imul(ecx).unwrap(); // one-op signed
            a.mov(esi, 0x0001_0000i32).unwrap();
            a.imul_2(esi, esi).unwrap(); // two-op, overflows
            a.imul_3(edi, esi, -3i32).unwrap(); // three-op
            a.mov(rax, 0x1_0000_0000u64).unwrap();
            a.mov(rbx, 0x1_0000_0000u64).unwrap();
            a.mul(rbx).unwrap(); // 64-bit -> rdx=1
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn div_idiv_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(edx, 0i32).unwrap();
            a.mov(eax, 1_000_000i32).unwrap();
            a.mov(ecx, 7i32).unwrap();
            a.div(ecx).unwrap(); // unsigned 32
            a.mov(edx, 0i32).unwrap();
            a.mov(rax, 0x1_0000_0000u64).unwrap();
            a.mov(rbx, 3u64).unwrap();
            a.div(rbx).unwrap(); // unsigned 64
            a.mov(eax, -1000i32).unwrap();
            a.mov(edx, -1i32).unwrap(); // sign-extend dividend
            a.mov(esi, 7i32).unwrap();
            a.idiv(esi).unwrap(); // signed 32
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn div_by_zero_raises_de() {
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(edx, 0i32).unwrap();
    asm.mov(eax, 10i32).unwrap();
    asm.mov(ecx, 0i32).unwrap();
    asm.div(ecx).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(
        VmConfig { memory_model: MemoryModel::Flat { size: 0x2000 }, consistency: MemConsistency::Fast },
        Box::new(JitBackend::new()),
    );
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    match cpu.run(&vm, Some(100)) {
        Exit::Exception { vector, .. } => assert_eq!(vector, 0, "#DE is vector 0"),
        other => panic!("expected #DE, got {other:?}"),
    }
}

#[test]
fn sse_movement_and_logic_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.movq(xmm0, rax).unwrap(); // gpr -> xmm (zero upper)
            a.mov(rbx, 0xAABB_CCDD_EEFF_0011u64).unwrap();
            a.movq(xmm1, rbx).unwrap();
            a.pxor(xmm2, xmm2).unwrap(); // zero
            a.por(xmm2, xmm0).unwrap();
            a.pand(xmm2, xmm1).unwrap();
            a.movdqu(xmmword_ptr(SCRATCH), xmm2).unwrap(); // store 128
            a.movdqu(xmm3, xmmword_ptr(SCRATCH)).unwrap(); // load 128
            a.movdqa(xmm4, xmm3).unwrap(); // reg-reg
            a.pandn(xmm0, xmm1).unwrap();
            a.movd(ecx, xmm3).unwrap(); // xmm -> gpr32
            a.movq(rdx, xmm1).unwrap(); // xmm -> gpr64
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn packed_arith_shift_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x0000_0002_0000_0001u64).unwrap();
            a.movq(xmm0, rax).unwrap();
            a.mov(rax, 0x0000_0004_0000_0003u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.paddd(xmm0, xmm1).unwrap();
            a.psubd(xmm1, xmm0).unwrap();
            a.pcmpeqd(xmm2, xmm2).unwrap();
            a.mov(rax, 0xFF00_FF00_FF00_FF00u64).unwrap();
            a.movq(xmm3, rax).unwrap();
            a.pslld(xmm3, 4).unwrap();
            a.psrld(xmm3, 8).unwrap();
            a.psrlw(xmm3, 2).unwrap();
            a.paddq(xmm0, xmm1).unwrap();
            a.paddw(xmm2, xmm3).unwrap();
            a.movdqa(xmm4, xmm3).unwrap();
            a.psrldq(xmm4, 3).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn float_scalar_match_interp() {
    jit_eq_interp(float_scalar_body, |_| {}, &[]);
}

#[test]
fn float_packed_match_interp() {
    jit_eq_interp(float_packed_body, |_| {}, &[]);
}

/// Scalar SSE2 double: cvtsi2sd/movsd/add/sub/mul/div, a memory source, both
/// convert-to-int roundings, precision converts, and a compare setting flags. All
/// values are exact IEEE doubles so the result is bit-stable across backends.
fn float_scalar_body(a: &mut CodeAssembler) {
    a.mov(rax, 7i64).unwrap();
    a.cvtsi2sd(xmm0, rax).unwrap(); // 7.0
    a.mov(rax, 2i64).unwrap();
    a.cvtsi2sd(xmm1, rax).unwrap(); // 2.0
    a.movsd_2(xmm2, xmm0).unwrap(); // 7.0 (reg merge)
    a.addsd(xmm2, xmm1).unwrap(); // 9.0
    a.subsd(xmm2, xmm0).unwrap(); // 2.0
    a.mulsd(xmm2, xmm0).unwrap(); // 14.0
    a.divsd(xmm2, xmm1).unwrap(); // 7.0
    a.mov(rax, 0x4008_0000_0000_0000u64).unwrap(); // 3.0
    a.mov(qword_ptr(SCRATCH), rax).unwrap();
    a.addsd(xmm2, qword_ptr(SCRATCH)).unwrap(); // 10.0 (mem source)
    a.cvttsd2si(rcx, xmm2).unwrap(); // 10
    // 3.5 -> trunc 3, round-half-to-even 4.
    a.mov(rax, 7i64).unwrap();
    a.cvtsi2sd(xmm3, rax).unwrap();
    a.divsd(xmm3, xmm1).unwrap(); // 3.5
    a.cvttsd2si(rdx, xmm3).unwrap(); // 3
    a.cvtsd2si(rsi, xmm3).unwrap(); // 4
    a.mov(rax, -5i64).unwrap();
    a.cvtsi2sd(xmm4, rax).unwrap(); // -5.0
    a.cvttsd2si(rdi, xmm4).unwrap(); // -5
    a.cvtsd2ss(xmm5, xmm2).unwrap(); // 10.0 -> f32
    a.cvtss2sd(xmm6, xmm5).unwrap(); // -> f64
    a.ucomisd(xmm0, xmm1).unwrap(); // 7 vs 2: CF=0 ZF=0 PF=0
    a.hlt().unwrap();
}

/// Packed double (mulpd/addpd/subpd + a memory source) and packed single
/// (mulps/addps/divps), plus scalar single and a `comiss` compare.
fn float_packed_body(a: &mut CodeAssembler) {
    // packed double [1.5, 2.5]
    a.mov(rax, 0x3FF8_0000_0000_0000u64).unwrap(); // 1.5
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x4004_0000_0000_0000u64).unwrap(); // 2.5
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap(); // [1.5, 2.5]
    a.movapd(xmm2, xmm0).unwrap();
    a.mulpd(xmm2, xmm0).unwrap(); // [2.25, 6.25]
    a.addpd(xmm2, xmm0).unwrap(); // [3.75, 8.75]
    a.subpd(xmm2, xmm0).unwrap(); // [2.25, 6.25]
    a.movupd(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.mulpd(xmm2, xmmword_ptr(SCRATCH)).unwrap(); // [3.375, 15.625] (mem source)
    // packed single [1,2,3,4]
    a.mov(rax, 0x4000_0000_3F80_0000u64).unwrap(); // 1.0, 2.0
    a.movq(xmm3, rax).unwrap();
    a.mov(rax, 0x4080_0000_4040_0000u64).unwrap(); // 3.0, 4.0
    a.movq(xmm4, rax).unwrap();
    a.punpcklqdq(xmm3, xmm4).unwrap(); // [1,2,3,4]
    a.mulps(xmm3, xmm3).unwrap(); // [1,4,9,16]
    a.addps(xmm3, xmm3).unwrap(); // [2,8,18,32]
    a.divps(xmm3, xmm3).unwrap(); // [1,1,1,1]
    // scalar single
    a.mov(rax, 9i64).unwrap();
    a.cvtsi2ss(xmm5, rax).unwrap(); // 9.0f
    a.mov(rax, 4i64).unwrap();
    a.cvtsi2ss(xmm6, rax).unwrap(); // 4.0f
    a.movss(xmm7, xmm5).unwrap();
    a.addss(xmm7, xmm6).unwrap(); // 13.0
    a.mulss(xmm7, xmm6).unwrap(); // 52.0
    a.subss(xmm7, xmm6).unwrap(); // 48.0
    a.divss(xmm7, xmm6).unwrap(); // 12.0
    a.cvttss2si(r10, xmm7).unwrap(); // 12
    a.comiss(xmm5, xmm6).unwrap(); // 9 vs 4: CF=0 ZF=0 PF=0
    a.hlt().unwrap();
}

#[test]
fn atomics_match_interp() {
    jit_eq_interp(atomics_body, |_| {}, &[]);
}

/// Locked RMW, xchg, xadd, and cmpxchg (success + failure) across byte/dword/qword
/// sizes. Memory effects are read back into registers so the snapshot observes
/// them; final flags come from the failing cmpxchg's compare.
fn atomics_body(a: &mut CodeAssembler) {
    a.mov(qword_ptr(SCRATCH), 100i32).unwrap();
    a.mov(rax, 5i64).unwrap();
    a.lock().add(qword_ptr(SCRATCH), rax).unwrap(); // mem = 105
    a.mov(rbx, 3i64).unwrap();
    a.lock().xadd(qword_ptr(SCRATCH), rbx).unwrap(); // rbx = 105 (old), mem = 108
    a.mov(r8, qword_ptr(SCRATCH)).unwrap(); // r8 = 108
    a.lock().inc(qword_ptr(SCRATCH)).unwrap(); // mem = 109
    a.lock().dec(qword_ptr(SCRATCH)).unwrap(); // mem = 108
    a.mov(r9, qword_ptr(SCRATCH)).unwrap(); // r9 = 108
    // atomic exchange (implicitly locked)
    a.mov(r10, 777i64).unwrap();
    a.xchg(qword_ptr(SCRATCH), r10).unwrap(); // r10 = 108 (old), mem = 777
    a.mov(r11, qword_ptr(SCRATCH)).unwrap(); // r11 = 777
    // dword lock or
    a.mov(dword_ptr(SCRATCH + 16), 0xF0i32).unwrap();
    a.mov(ecx, 0x0Fi32).unwrap();
    a.lock().or(dword_ptr(SCRATCH + 16), ecx).unwrap(); // mem32 = 0xFF
    a.mov(r14d, dword_ptr(SCRATCH + 16)).unwrap();
    // cmpxchg success
    a.mov(qword_ptr(SCRATCH), 42i32).unwrap();
    a.mov(rax, 42i64).unwrap();
    a.mov(rsi, 99i64).unwrap();
    a.lock().cmpxchg(qword_ptr(SCRATCH), rsi).unwrap(); // match: mem = 99, ZF = 1, rax = 42
    a.mov(r12, qword_ptr(SCRATCH)).unwrap(); // r12 = 99
    // byte lock add (al = rax low byte = 42)
    a.mov(byte_ptr(SCRATCH + 24), 1i32).unwrap();
    a.lock().add(byte_ptr(SCRATCH + 24), al).unwrap(); // 1 + 42 = 43
    a.movzx(r15, byte_ptr(SCRATCH + 24)).unwrap(); // r15 = 43
    // cmpxchg failure (rax = 7 != mem 99)
    a.mov(rax, 7i64).unwrap();
    a.mov(rdi, 123i64).unwrap();
    a.lock().cmpxchg(qword_ptr(SCRATCH), rdi).unwrap(); // mismatch: rax = 99, ZF = 0
    a.mov(r13, qword_ptr(SCRATCH)).unwrap(); // r13 = 99 (unchanged)
    a.hlt().unwrap();
}

#[test]
fn string_ops_match_interp() {
    jit_eq_interp(
        |a| {
            a.cld().unwrap();
            a.mov(edi, SCRATCH as i32).unwrap();
            a.mov(ecx, 12i32).unwrap();
            a.mov(eax, 0xA5i32).unwrap();
            a.rep().stosb().unwrap();
            a.mov(esi, SCRATCH as i32).unwrap();
            a.mov(edi, (SCRATCH + 64) as i32).unwrap();
            a.mov(ecx, 3i32).unwrap();
            a.rep().movsq().unwrap(); // 24 bytes qword copy
            a.mov(edi, SCRATCH as i32).unwrap();
            a.mov(ecx, 12i32).unwrap();
            a.mov(al, 0xA5i32).unwrap();
            a.repne().scasb().unwrap();
            a.std().unwrap();
            a.mov(esi, (SCRATCH + 8) as i32).unwrap();
            a.mov(edi, (SCRATCH + 128) as i32).unwrap();
            a.mov(ecx, 4i32).unwrap();
            a.rep().movsb().unwrap(); // backward copy (DF=1)
            a.cld().unwrap();
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
                if shim.handle(&mut cpu, &mut vm) {
                    break;
                }
            }
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    (shim.stdout, shim.exit_code)
}

/// Preemption under chaining (M5-T1-preempt, §9.2): a tight chained loop must
/// still honor the block budget, or it would starve other vcpus at M7.
#[test]
fn chained_loop_still_yields_budget() {
    // Infinite loop: jmp self.
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    asm.set_label(&mut top).unwrap();
    asm.jmp(top).unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(
        VmConfig { memory_model: MemoryModel::Flat { size: 0x2000 }, consistency: MemConsistency::Fast },
        Box::new(JitBackend::new()),
    );
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);

    // Would spin forever without preemption; the budget stops it.
    assert!(matches!(cpu.run(&vm, Some(1000)), Exit::BudgetExhausted));
}

/// Block chaining "fires" (M5, testing.md §8.2): a JIT loop must take the chained
/// link-slot path, not re-dispatch every iteration. Catches a silent no-op where
/// chaining does nothing yet still passes correctness ("nothing changed").
#[test]
fn chaining_fires_on_a_loop() {
    let mut asm = CodeAssembler::new(64).unwrap();
    let mut top = asm.create_label();
    asm.mov(ecx, 1000i32).unwrap();
    asm.set_label(&mut top).unwrap();
    asm.sub(ecx, 1i32).unwrap();
    asm.jnz(top).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(
        VmConfig { memory_model: MemoryModel::Flat { size: 0x2000 }, consistency: MemConsistency::Fast },
        Box::new(JitBackend::new()),
    );
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(&vm, Some(100_000)), Exit::Hlt));

    // The loop back-edge chains every iteration after it's linked.
    assert!(vm.cache.chained() > 500, "chaining didn't fire: {}", vm.cache.chained());
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
