//! JIT config-matrix acceptance (M4, testing.md §8.1): the Cranelift backend must
//! produce identical state to the interpreter on every input. The interpreter is
//! the oracle for the JIT (§8).

use iced_x86::code_asm::*;
use x86jit_core::jit_abi::run_compiled;
use x86jit_core::lift::{lift_block, LiftError};
use x86jit_core::GuestCpuFeatures;
use x86jit_core::{
    CachedBlock, Exit, InterpreterBackend, Prot, Reg, RegionKind, StepResult, Vm, VmConfig,
};
use x86jit_cranelift::{HostTarget, JitBackend};
use x86jit_tests::compare::{check, compare};
use x86jit_tests::oracle::{
    run_with_backend, run_with_backend_features, InterpreterOracle, Oracle, VectorInput,
};
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
    jit_eq_interp_features(GuestCpuFeatures::default(), build, init, dont_care);
}

/// As [`jit_eq_interp`] but with an explicit guest CPU feature set (task-169), so an
/// AVX-512 snippet can run under `GuestCpuFeatures::v4()`.
fn jit_eq_interp_features(
    features: GuestCpuFeatures,
    build: impl FnOnce(&mut CodeAssembler),
    init: impl FnOnce(&mut CpuSnapshot),
    dont_care: &[FlagName],
) {
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();

    let mut cpu = CpuSnapshot {
        rip: CODE,
        ..Default::default()
    };
    init(&mut cpu);

    let input = VectorInput {
        cpu_init: cpu,
        mem_init: vec![
            MemChunk {
                addr: CODE,
                bytes: code,
                kind: MemKind::Ram,
            },
            MemChunk {
                addr: SCRATCH,
                bytes: vec![0u8; SCRATCH_LEN],
                kind: MemKind::Ram,
            },
        ],
        entry: CODE,
        run: RunSpec::UntilExit,
    };

    let interp = run_with_backend_features(&input, Box::new(InterpreterBackend), features);
    let jit = run_with_backend_features(&input, Box::new(JitBackend::new()), features);
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

/// Prefetch (`0F 18`) is a pure cache hint: it lifts to a no-op, never faults on its
/// memory operand, and execution continues past it identically under interp and JIT.
/// Go's runtime memmove emits it — real caddy trapped here (task-153).
#[test]
fn prefetch_is_a_noop() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.prefetcht0(byte_ptr(rax)).unwrap();
            a.prefetchnta(byte_ptr(rax + 8)).unwrap();
            a.prefetchw(byte_ptr(rax + 16)).unwrap();
            a.mov(ecx, 42i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn fwait_is_a_noop() {
    // 0x9B (FWAIT/WAIT) lifts to zero IR ops, so the JIT must produce the same
    // state as the interpreter with no codegen for it (task-194).
    jit_eq_interp(
        |a| {
            a.mov(eax, 41i32).unwrap();
            a.wait().unwrap(); // 0x9B
            a.inc(eax).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
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

    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(JitBackend::new()));
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
fn idiv_overflow_raises_de() {
    // 64-bit `idiv` of RDX:RAX = i128::MIN by -1: the quotient (2^127) overflows a
    // signed 64-bit result, so the architecture raises #DE. Regression for the
    // `divide()` checked-div fix — before it, this panicked the host process
    // ("attempt to divide with overflow") instead of yielding an exception exit.
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(rdx, 0x8000_0000_0000_0000u64).unwrap();
    asm.xor(eax, eax).unwrap(); // RAX = 0 -> RDX:RAX = i128::MIN
    asm.mov(rcx, -1i64).unwrap();
    asm.idiv(rcx).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(JitBackend::new()));
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    match cpu.run(&vm, Some(100)) {
        Exit::Exception { vector, .. } => assert_eq!(vector, 0, "#DE is vector 0"),
        other => panic!("expected #DE, got {other:?}"),
    }
}

/// `ud2`/`int3`/`int1` are architectural exceptions, not lift gaps: they must
/// surface as `Exit::Exception` with the right vector (`#UD`=6, `#BP`=3, `#DB`=1),
/// NOT `Exit::UnknownInstruction`. Pinned under both backends so interp and JIT agree
/// on the vector carried out through the MemCtx out-field.
fn assert_trap_vector(code: &[u8], expected: u8, expected_rip: u64, jit: bool) {
    let backend: Box<dyn x86jit_core::Backend> = if jit {
        Box::new(JitBackend::new())
    } else {
        Box::new(InterpreterBackend)
    };
    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), backend);
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    match cpu.run(&vm, Some(100)) {
        Exit::Exception { vector, addr } => {
            assert_eq!(vector, expected, "trap vector (jit={jit})");
            // x86 saved-RIP: on the instruction for a fault, past it for a trap.
            assert_eq!(addr, expected_rip, "saved RIP (jit={jit})");
            assert_eq!(cpu.reg(Reg::Rip), expected_rip, "vcpu RIP (jit={jit})");
        }
        other => panic!("expected Exception vector {expected} (jit={jit}), got {other:?}"),
    }
}

#[test]
fn ud2_raises_ud() {
    let code = [0x0f, 0x0b]; // ud2 — fault, RIP stays on the instruction
    assert_trap_vector(&code, 6, CODE, false);
    assert_trap_vector(&code, 6, CODE, true);
}

#[test]
fn int3_raises_bp() {
    let code = [0xcc]; // int3 — trap, RIP resumes past the 1-byte instruction
    assert_trap_vector(&code, 3, CODE + 1, false);
    assert_trap_vector(&code, 3, CODE + 1, true);
}

#[test]
fn int1_raises_db() {
    let code = [0xf1]; // int1 (icebp) — trap, RIP resumes past it
    assert_trap_vector(&code, 1, CODE + 1, false);
    assert_trap_vector(&code, 1, CODE + 1, true);
}

// The in-span-but-unmapped interp/JIT oracle gap (decision-3) is closed for every
// host-backed span by guard pages (doc-30, decision-7): the runner's non-Go Flat and
// Go Reserved paths both fault `UnmappedMemory` under the JIT now, pinned in
// `x86jit-tests/tests/guard_pages.rs`. A `Vec`-backed VM built by `Vm::with_backend`
// (test-only — no host pages to `mprotect`) still can't guard, but no real guest is
// Vec-backed, so there is nothing left to pin here.

#[test]
fn unknown_instruction_reports_real_bytes() {
    // An unlifted instruction (`dpps`, the SSE4.1 dot-product we deliberately do not
    // lift) must surface its actual opcode bytes in the lift error, not 15 zeros — so
    // compat triage isn't misdirected (#18). `ptest`, then `pcmpistri`, used to sit here
    // but are now lifted (task-168.4 / task-168.5.4).
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.dpps(xmm0, xmm1, 0).unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    match lift_block(&vm.mem, CODE) {
        Err(LiftError::Unsupported { bytes, len, .. }) => {
            assert_ne!(bytes, [0u8; 15], "must not report 15 zero bytes");
            assert_eq!(
                &bytes[..len as usize],
                &code[..len as usize],
                "reported bytes must be the real dpps opcode"
            );
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn run_compiled_decodes_exception_not_panic() {
    // The `run_compiled` convenience helper must decode RET_EXCEPTION to
    // `Exit::Exception`, not fall through to its `panic!` (#15B). Materialize a #DE
    // (idiv overflow) block and run it through the helper directly.
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(rdx, 0x8000_0000_0000_0000u64).unwrap();
    asm.xor(eax, eax).unwrap();
    asm.mov(rcx, -1i64).unwrap();
    asm.idiv(rcx).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(JitBackend::new()));
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    let ir = lift_block(&vm.mem, CODE).expect("lift the block");
    let entry = match vm.backend.materialize(
        &ir,
        vm.consistency,
        vm.mem.trap_window(),
        vm.mem.guest_base(),
    ) {
        CachedBlock::Compiled { entry, .. } => entry,
        _ => panic!("JIT backend must compile the block"),
    };
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    // SAFETY: `entry` is a freshly compiled block for `vm`'s memory, run once.
    match unsafe { run_compiled(entry, &mut cpu.cpu, &vm.mem) } {
        StepResult::Exit(Exit::Exception { vector, .. }) => {
            assert_eq!(vector, 0, "#DE is vector 0")
        }
        StepResult::Exit(e) => panic!("expected an exception exit, got {e:?}"),
        StepResult::Continue => panic!("expected an exception exit, got Continue"),
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
            a.movdqa(xmm5, xmm3).unwrap();
            a.pslldq(xmm5, 4).unwrap(); // byte-shift left (ld.so strcmp path)
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
                                   // min/max (scalar + packed) and sqrt
    a.minsd(xmm2, xmm0).unwrap(); // min([3.375,15.625],[1.5,2.5]) scalar -> lane0 min(3.375,1.5)=1.5
    a.maxpd(xmm0, xmm1).unwrap(); // packed max([1.5,2.5],[2.5,2.5])? xmm1=[2.5,?]
    a.minps(xmm3, xmm4).unwrap(); // packed
    a.maxss(xmm5, xmm6).unwrap(); // scalar max(9,4)=9
    a.mov(rax, 16i64).unwrap();
    a.cvtsi2sd(xmm8, rax).unwrap(); // 16.0
    a.sqrtsd(xmm9, xmm8).unwrap(); // 4.0
    a.sqrtss(xmm10, xmm5).unwrap(); // sqrt(9)=3
    a.xorpd(xmm11, xmm11).unwrap(); // zero via pd-logic alias
    a.hlt().unwrap();
}

#[test]
fn atomics_match_interp() {
    jit_eq_interp(atomics_body, |_| {}, &[]);
}

/// `lock bts/btr/btc [mem], reg|imm` (task-117): the locked memory bit-ops now lift to
/// an atomic RMW. Single-threaded this can't observe the atomicity, but it pins that
/// the atomic path (mask + `AtomicRmw` + CF-from-old) produces the same memory result
/// and CF as the plain load-modify-store — across both the register-index (byte-string
/// addressing) and immediate-index (operand-width) forms, JIT == interp.
fn locked_bit_ops_body(a: &mut CodeAssembler) {
    a.mov(dword_ptr(SCRATCH), 0b1010i32).unwrap(); // bits 1 and 3 set
                                                   // register-index → byte-string addressing; each `setb` captures the pre-op bit (CF).
    a.mov(ecx, 5i32).unwrap();
    a.lock().bts(dword_ptr(SCRATCH), ecx).unwrap(); // set bit 5 (was 0 → CF 0)
    a.setb(r8b).unwrap();
    a.mov(edx, 1i32).unwrap();
    a.lock().btr(dword_ptr(SCRATCH), edx).unwrap(); // reset bit 1 (was 1 → CF 1)
    a.setb(r9b).unwrap();
    a.mov(esi, 3i32).unwrap();
    a.lock().btc(dword_ptr(SCRATCH), esi).unwrap(); // flip bit 3 (was 1 → CF 1)
    a.setb(r10b).unwrap();
    // immediate index → operand-width access, locked + non-locked.
    a.lock().bts(dword_ptr(SCRATCH), 6i32).unwrap(); // set bit 6 (was 0 → CF 0)
    a.setb(r11b).unwrap();
    a.btc(dword_ptr(SCRATCH), 3i32).unwrap(); // flip bit 3 again (non-atomic path)
    a.setb(r13b).unwrap();
    a.mov(r12d, dword_ptr(SCRATCH)).unwrap();
    a.hlt().unwrap();
}

#[test]
fn locked_bit_ops_match_interp() {
    jit_eq_interp(locked_bit_ops_body, |_| {}, &[]);
}

#[test]
fn bit_test_match_interp() {
    jit_eq_interp(bit_test_body, |_| {}, &[]);
}

#[test]
fn bitscan_and_cdq_match_interp() {
    jit_eq_interp(bitscan_cdq_body, |_| {}, &[]);
}

#[test]
fn x87_match_interp() {
    jit_eq_interp(x87_body, |_| {}, &[]);
}

/// x87 stack arithmetic, int/float load-store, fchs/fabs, and a compare — all on
/// exactly-representable values, so the f64 backing equals true 80-bit. Results
/// are read back into registers (the snapshot doesn't cover the x87 stack).
fn x87_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x4008_0000_0000_0000u64).unwrap(); // 3.0
    a.mov(qword_ptr(SCRATCH), rax).unwrap();
    a.mov(rax, 0x4010_0000_0000_0000u64).unwrap(); // 4.0
    a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
    a.fld(qword_ptr(SCRATCH)).unwrap(); // 3
    a.fld(qword_ptr(SCRATCH + 8)).unwrap(); // 4, 3
    a.faddp(st1, st0).unwrap(); // 7
    a.fld1().unwrap();
    a.fld1().unwrap();
    a.faddp(st1, st0).unwrap(); // 2, 7
    a.fmulp(st1, st0).unwrap(); // 14
    a.fld1().unwrap();
    a.fsubp(st1, st0).unwrap(); // 13
    a.fst(qword_ptr(SCRATCH + 16)).unwrap(); // 13.0 (no pop)
    a.fistp(qword_ptr(SCRATCH + 24)).unwrap(); // int 13, pop
    a.mov(r8, qword_ptr(SCRATCH + 16)).unwrap();
    a.mov(r9, qword_ptr(SCRATCH + 24)).unwrap();
    a.mov(dword_ptr(SCRATCH + 32), 5i32).unwrap();
    a.fild(dword_ptr(SCRATCH + 32)).unwrap(); // 5
    a.fchs().unwrap(); // -5
    a.fabs().unwrap(); // 5
    a.fistp(dword_ptr(SCRATCH + 36)).unwrap();
    a.mov(r10d, dword_ptr(SCRATCH + 36)).unwrap();
    a.fld1().unwrap(); // 1
    a.fldz().unwrap(); // 0, 1
    a.fucomip(st0, st1).unwrap(); // 0 vs 1 -> CF=1, pop
    a.setb(r11b).unwrap();
    a.hlt().unwrap();
}

#[test]
fn sse_half_moves_match_interp() {
    jit_eq_interp(sse_half_body, |_| {}, &[]);
}

#[test]
fn sse_string_ops_match_interp() {
    jit_eq_interp(sse_string_body, |_| {}, &[]);
}

#[test]
fn sse_shuffle_cmp_match_interp() {
    jit_eq_interp(sse_shuffle_cmp_body, |_| {}, &[]);
}

/// shufps/shufpd, cmp{ss,sd,ps,pd} (a few predicates), psraw/psrad, punpckh*, and
/// pshufd with a memory source — the SSE ops CPython pulls in. Results read into
/// registers; float compares use exact values.
fn sse_shuffle_cmp_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x0706_0504_0302_0100u64).unwrap();
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x0F0E_0D0C_0B0A_0908u64).unwrap();
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap(); // [0..15]
    a.movdqa(xmm2, xmm0).unwrap();
    a.shufps(xmm2, xmm0, 0x1B).unwrap(); // reverse 32-bit lanes
    a.movq(r8, xmm2).unwrap();
    a.movdqa(xmm3, xmm0).unwrap();
    a.shufpd(xmm3, xmm0, 0x1).unwrap();
    a.movq(r9, xmm3).unwrap();
    // punpckh* (high unpack)
    a.movdqa(xmm4, xmm0).unwrap();
    a.punpckhbw(xmm4, xmm1).unwrap();
    a.movq(r10, xmm4).unwrap();
    a.movdqa(xmm5, xmm0).unwrap();
    a.punpckhwd(xmm5, xmm1).unwrap();
    a.movq(r11, xmm5).unwrap();
    a.movdqa(xmm6, xmm0).unwrap();
    a.punpckhdq(xmm6, xmm1).unwrap();
    a.movq(r12, xmm6).unwrap();
    // psraw / psrad (arithmetic right)
    a.mov(rax, 0x8000_4000_FF00_0100u64).unwrap();
    a.movq(xmm7, rax).unwrap();
    a.movdqa(xmm8, xmm7).unwrap();
    a.psraw(xmm8, 4).unwrap();
    a.movq(r13, xmm8).unwrap();
    a.movdqa(xmm9, xmm7).unwrap();
    a.psrad(xmm9, 20).unwrap();
    a.movq(r14, xmm9).unwrap();
    // scalar double compare (predicate 1 = LT) via cvtsi2sd
    a.mov(rax, 3i64).unwrap();
    a.cvtsi2sd(xmm10, rax).unwrap();
    a.mov(rax, 5i64).unwrap();
    a.cvtsi2sd(xmm11, rax).unwrap();
    a.cmpltsd(xmm10, xmm11).unwrap(); // 3 < 5 -> all-ones mask
    a.movq(r15, xmm10).unwrap();
    // pshufd with a memory source
    a.movdqu(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.pshufd(xmm12, xmmword_ptr(SCRATCH), 0x1B).unwrap();
    a.movq(rbx, xmm12).unwrap();
    a.hlt().unwrap();
}

/// The SSE2 ops glibc's string routines lean on: pmovmskb, packed unsigned/signed
/// min/max, pcmpgt, and movlpd/movhpd. Results are read back into registers.
fn sse_string_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x8000_7F01_0080_00FFu64).unwrap();
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x0102_8304_0586_0708u64).unwrap();
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap();
    a.pmovmskb(ecx, xmm0).unwrap(); // MSB of each byte
                                    // packed min/max
    a.mov(rax, 0x1020_3040_5060_7080u64).unwrap();
    a.movq(xmm2, rax).unwrap();
    a.mov(rax, 0x151F_353F_555F_757Fu64).unwrap();
    a.movq(xmm3, rax).unwrap();
    a.movdqa(xmm4, xmm2).unwrap();
    a.pminub(xmm4, xmm3).unwrap();
    a.movq(r8, xmm4).unwrap();
    a.movdqa(xmm5, xmm2).unwrap();
    a.pmaxub(xmm5, xmm3).unwrap();
    a.movq(r9, xmm5).unwrap();
    a.movdqa(xmm6, xmm2).unwrap();
    a.pminsw(xmm6, xmm3).unwrap();
    a.movq(r10, xmm6).unwrap();
    a.movdqa(xmm7, xmm2).unwrap();
    a.pmaxsw(xmm7, xmm3).unwrap();
    a.movq(r11, xmm7).unwrap();
    // pcmpgt (signed)
    a.movdqa(xmm8, xmm2).unwrap();
    a.pcmpgtb(xmm8, xmm3).unwrap();
    a.movq(r12, xmm8).unwrap();
    a.movdqa(xmm9, xmm2).unwrap();
    a.pcmpgtd(xmm9, xmm3).unwrap();
    a.movq(r13, xmm9).unwrap();
    // movhpd / movlpd (memory)
    a.movdqu(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.movhpd(xmm10, qword_ptr(SCRATCH)).unwrap();
    a.movq(r14, xmm10).unwrap(); // low half unchanged (0), so this reads 0
    a.pshufd(xmm10, xmm10, 0x4E).unwrap(); // swap halves to observe the loaded high
    a.movq(r15, xmm10).unwrap();
    a.hlt().unwrap();
}

/// cwd/cdq/cqo sign-extension and bsf/bsr (including the src==0 → ZF case, where
/// the destination is preserved). ZF captured via `setz`.
fn bitscan_cdq_body(a: &mut CodeAssembler) {
    a.mov(eax, 0x8000_0000u32 as i32).unwrap();
    a.cdq().unwrap(); // edx = 0xFFFFFFFF
    a.mov(r8d, edx).unwrap();
    a.mov(eax, 0x4000_0000i32).unwrap();
    a.cdq().unwrap(); // edx = 0
    a.mov(r9d, edx).unwrap();
    a.mov(eax, 0x0000_0100i32).unwrap();
    a.bsf(ebx, eax).unwrap(); // 8
    a.bsr(r10d, eax).unwrap(); // 8
    a.mov(rax, 0x8000_0000_0000_0000u64).unwrap();
    a.bsr(r11, rax).unwrap(); // 63
    a.bsf(r12, rax).unwrap(); // 63
    a.mov(r13, 0xDEADu64).unwrap();
    a.mov(esi, 0i32).unwrap();
    a.bsf(r13d, esi).unwrap(); // src==0: ZF=1, r13 preserved (low 32 = 0xDEAD)
    a.setz(r14b).unwrap();
    a.mov(eax, 1i32).unwrap();
    a.bsf(ebp, eax).unwrap(); // 0, ZF=0
    a.setz(r15b).unwrap();
    a.hlt().unwrap();
}

/// pshuflw/pshufhw, pextrw, movlhps/movhlps, and movhps/movlps (mem load + store).
fn sse_half_body(a: &mut CodeAssembler) {
    a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
    a.movq(xmm0, rax).unwrap();
    a.mov(rax, 0x99AA_BBCC_DDEE_FF00u64).unwrap();
    a.movq(xmm1, rax).unwrap();
    a.punpcklqdq(xmm0, xmm1).unwrap(); // [0x11..88, 0x99..00]
    a.pshuflw(xmm2, xmm0, 0x1Bi32).unwrap(); // reverse low 4 words
    a.pshufhw(xmm3, xmm0, 0x1Bi32).unwrap(); // reverse high 4 words
    a.pextrw(ecx, xmm0, 3i32).unwrap();
    a.movlhps(xmm4, xmm0).unwrap(); // xmm4 high = xmm0 low
    a.movhlps(xmm5, xmm0).unwrap(); // xmm5 low = xmm0 high
    a.movdqu(xmmword_ptr(SCRATCH), xmm0).unwrap();
    a.movhps(xmm6, qword_ptr(SCRATCH)).unwrap(); // load high half from mem
    a.movlps(xmm7, qword_ptr(SCRATCH + 8)).unwrap(); // load low half from mem
    a.movhps(qword_ptr(SCRATCH + 16), xmm0).unwrap(); // store high half
    a.movlps(qword_ptr(SCRATCH + 32), xmm0).unwrap(); // store low half
    a.mov(r8, qword_ptr(SCRATCH + 16)).unwrap();
    a.mov(r9, qword_ptr(SCRATCH + 32)).unwrap();
    a.hlt().unwrap();
}

#[test]
fn cpuid_match_interp() {
    // cpuid reports engine-chosen features (not the host's), so it's validated
    // interp-vs-JIT only — never against Unicorn/the real CPU.
    jit_eq_interp(
        |a| {
            a.mov(eax, 0i32).unwrap();
            a.cpuid().unwrap();
            a.mov(r8d, ebx).unwrap(); // vendor "Genu"
            a.mov(r9d, edx).unwrap();
            a.mov(eax, 1i32).unwrap();
            a.xor(ecx, ecx).unwrap();
            a.cpuid().unwrap();
            a.mov(r10d, edx).unwrap(); // feature flags (SSE2 etc.)
            a.mov(r11d, ecx).unwrap(); // 0 (no SSE3+/AVX)
            a.mov(eax, 7i32).unwrap();
            a.xor(ecx, ecx).unwrap();
            a.cpuid().unwrap();
            a.mov(r12d, ebx).unwrap(); // 0 (no SHA/AVX2)
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// bt/bts/btr/btc with register and immediate indices, register and memory
/// operands. CF is captured per-op via `setb`; writebacks are read into registers.
fn bit_test_body(a: &mut CodeAssembler) {
    a.mov(rax, 0xAi64).unwrap(); // 1010b
    a.bt(rax, 3i32).unwrap(); // bit3 = 1 -> CF=1
    a.setb(r8b).unwrap();
    a.bt(rax, 2i32).unwrap(); // bit2 = 0 -> CF=0
    a.setb(r9b).unwrap();
    a.mov(rcx, 1i64).unwrap();
    a.bt(rax, rcx).unwrap(); // register index: bit1 = 1 -> CF=1
    a.setb(r10b).unwrap();
    a.bts(rax, 0i32).unwrap(); // set bit0 -> rax=0xB, CF=old bit0=0
    a.setb(r11b).unwrap();
    a.mov(rdx, rax).unwrap(); // 0xB
    a.btr(rax, 1i32).unwrap(); // clear bit1 -> rax=0x9, CF=1
    a.setb(r12b).unwrap();
    a.mov(rsi, rax).unwrap(); // 0x9
    a.btc(rax, 2i32).unwrap(); // toggle bit2 -> rax=0xD, CF=old bit2=0
    a.setb(r13b).unwrap();
    a.mov(rdi, rax).unwrap(); // 0xD
    a.mov(qword_ptr(SCRATCH), 0xF0i32).unwrap();
    a.bt(qword_ptr(SCRATCH), 5i32).unwrap(); // bit5 of 0xF0 = 1 -> CF=1
    a.setb(r14b).unwrap();
    a.bts(qword_ptr(SCRATCH), 0i32).unwrap(); // set bit0 -> mem=0xF1
    a.mov(r15, qword_ptr(SCRATCH)).unwrap(); // 0xF1
    a.hlt().unwrap();
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
    let (stdout, code) =
        run_program_on_jit(include_bytes!("../programs/hello_static.elf"), &[b"hello"]);
    assert_eq!(stdout, b"hello\n");
    assert_eq!(code, Some(0));
}

#[test]
fn echo_argv_runs_on_jit() {
    let (stdout, code) = run_program_on_jit(
        include_bytes!("../programs/echo_argv.elf"),
        &[b"echo_argv", b"WORLD"],
    );
    assert_eq!(stdout, b"WORLD");
    assert_eq!(code, Some(2));
}

fn run_program_on_jit(image: &[u8], argv: &[&[u8]]) -> (Vec<u8>, Option<i32>) {
    use x86jit_elf::{load_static_elf, setup_stack};

    const FLAT: u64 = 0x50_0000;
    const STACK_BASE: u64 = 0x48_0000;
    const STACK_TOP: u64 = 0x50_0000;

    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(JitBackend::new()));
    let entry = load_static_elf(&mut vm, image).unwrap();
    vm.map(
        STACK_BASE,
        (STACK_TOP - STACK_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
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

    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(JitBackend::new()));
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

    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(JitBackend::new()));
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    assert!(matches!(cpu.run(&vm, Some(100_000)), Exit::Hlt));

    // The loop back-edge chains every iteration after it's linked.
    assert!(
        vm.cache.chained() > 500,
        "chaining didn't fire: {}",
        vm.cache.chained()
    );
}

/// Direct-call chaining (fast-dispatch R2): a loop that `call`s a leaf subroutine every
/// iteration must chain the call edge (callee entry) through a link slot, not
/// re-dispatch. The loop has one back-edge (`jnz`) and one `call` per iteration;
/// with call chaining the "fires" counter roughly doubles vs the back-edge alone,
/// so a count well above the iteration count proves the call edge chains too.
/// Result correctness (sum 1000..=1) guards the control flow end to end.
#[test]
fn direct_call_chains_the_callee_edge() {
    // mov ecx,1000; mov eax,0; loop: call sub; add eax,ecx; sub ecx,1; jnz loop;
    // hlt; sub: ret   — eax accumulates 1000+999+...+1 = 500500.
    let build = |a: &mut CodeAssembler| {
        let mut top = a.create_label();
        let mut sub = a.create_label();
        a.mov(ecx, 1000i32).unwrap();
        a.mov(eax, 0i32).unwrap();
        a.set_label(&mut top).unwrap();
        a.call(sub).unwrap();
        a.add(eax, ecx).unwrap();
        a.sub(ecx, 1i32).unwrap();
        a.jnz(top).unwrap();
        a.hlt().unwrap();
        a.set_label(&mut sub).unwrap();
        a.ret().unwrap();
    };
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x1_0000), Box::new(JitBackend::new()));
    vm.map(0, 0x1_0000, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rsp, 0x8000);
    assert!(matches!(cpu.run(&vm, Some(100_000)), Exit::Hlt));

    assert_eq!(cpu.reg(Reg::Rax) as u32, 500_500, "call/ret loop result");
    // Back-edge alone would give ~1000; the call edge chaining pushes it well past.
    assert!(
        vm.cache.chained() > 1500,
        "direct call edge didn't chain: {}",
        vm.cache.chained()
    );
}

/// `Blocks(n)` exactness with a `call` in the loop body (fast-dispatch R2 preserves
/// §9.2): a chained call must tick the block budget exactly like the interpreter,
/// so a partial budget stops both backends at the identical guest state.
#[test]
fn call_loop_budget_stops_at_the_same_state() {
    let build = |a: &mut CodeAssembler| {
        let mut top = a.create_label();
        let mut sub = a.create_label();
        a.mov(ecx, 1000i32).unwrap();
        a.mov(eax, 0i32).unwrap();
        a.set_label(&mut top).unwrap();
        a.call(sub).unwrap();
        a.add(eax, ecx).unwrap();
        a.sub(ecx, 1i32).unwrap();
        a.jnz(top).unwrap();
        a.hlt().unwrap();
        a.set_label(&mut sub).unwrap();
        a.ret().unwrap();
    };
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();

    let run = |backend: Box<dyn x86jit_core::Backend>| {
        let mut vm = Vm::with_backend(VmConfig::flat(0x1_0000), backend);
        vm.map(0, 0x1_0000, Prot::RW, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        cpu.set_reg(Reg::Rsp, 0x8000);
        // Mid-run budget: enough to spin the loop but stop well before hlt.
        let exit = cpu.run(&vm, Some(777));
        assert!(matches!(exit, Exit::BudgetExhausted));
        (
            cpu.reg(Reg::Rax),
            cpu.reg(Reg::Rcx),
            cpu.reg(Reg::Rsp),
            cpu.reg(Reg::Rip),
        )
    };

    let interp = run(Box::new(InterpreterBackend));
    let jit = run(Box::new(JitBackend::new()));
    assert_eq!(
        jit, interp,
        "JIT and interpreter must stop at the same state"
    );
}

/// Build a `Vm`, run `build`'s program from CODE to `Hlt` on `backend`, and return
/// the finished vm + vcpu so counters and registers can be inspected.
fn run_flat_to_hlt(
    build: impl FnOnce(&mut CodeAssembler),
    backend: Box<dyn x86jit_core::Backend>,
) -> (Vm, x86jit_core::Vcpu) {
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x1_0000), backend);
    vm.map(0, 0x1_0000, Prot::RW, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    cpu.set_reg(Reg::Rsp, 0x8000);
    assert!(matches!(cpu.run(&vm, Some(1_000_000)), Exit::Hlt));
    (vm, cpu)
}

/// Call/return-heavy microbenchmark for the fast-dispatch track (R6).
/// Ignored by default (timing is machine-dependent); run with
/// `cargo test -p x86jit-tests --test jit --release fast_dispatch_call_bench -- --ignored --nocapture`.
/// Recursive Fibonacci is almost pure call/ret, so it isolates the dispatch cost
/// the track attacks. Prints JIT vs interpreter wall-clock and the fast-dispatch
/// counters (chained transfers, IBTC fills, fast-resolve hits) proving the
/// mechanisms fire.
#[test]
#[ignore]
fn fast_dispatch_call_bench() {
    // fib(n) by recursion: fib: cmp edi,2; jb base; push, edi-1, call, save, edi-2,
    // call, add; ret. Guest computes fib(N) into eax.
    const N: i32 = 32;
    let build = |a: &mut CodeAssembler| {
        let mut fib = a.create_label();
        let mut base = a.create_label();
        a.mov(edi, N).unwrap();
        a.call(fib).unwrap();
        a.hlt().unwrap();
        // fib(edi) -> eax
        a.set_label(&mut fib).unwrap();
        a.cmp(edi, 2i32).unwrap();
        a.jb(base).unwrap();
        a.push(rdi).unwrap(); // save n
        a.sub(edi, 1i32).unwrap();
        a.call(fib).unwrap(); // fib(n-1)
        a.pop(rdi).unwrap(); // restore n
        a.push(rax).unwrap(); // save fib(n-1)
        a.sub(edi, 2i32).unwrap();
        a.call(fib).unwrap(); // fib(n-2)
        a.pop(rcx).unwrap(); // fib(n-1)
        a.add(eax, ecx).unwrap(); // fib(n-1)+fib(n-2)
        a.ret().unwrap();
        a.set_label(&mut base).unwrap();
        a.mov(eax, edi).unwrap(); // fib(0)=0, fib(1)=1
        a.ret().unwrap();
    };

    let time = |backend: Box<dyn x86jit_core::Backend>| {
        let mut asm = CodeAssembler::new(64).unwrap();
        build(&mut asm);
        let code = asm.assemble(CODE).unwrap();
        let mut vm = Vm::with_backend(VmConfig::flat(0x10_0000), backend);
        vm.map(0, 0x10_0000, Prot::RW, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        cpu.set_reg(Reg::Rsp, 0x8_0000);
        let t0 = std::time::Instant::now();
        assert!(matches!(cpu.run(&vm, None), Exit::Hlt));
        let dt = t0.elapsed();
        (dt, cpu.reg(Reg::Rax) as u32, vm, cpu)
    };

    let (it, ires, _iv, _ic) = time(Box::new(InterpreterBackend));
    let (jt, jres, jv, jc) = time(Box::new(JitBackend::new()));
    assert_eq!(ires, jres, "interp and JIT must agree on fib({N})");
    println!("fib({N}) = {jres}");
    println!("  interp : {it:?}");
    println!(
        "  jit    : {jt:?}  ({:.2}x over interp)",
        it.as_secs_f64() / jt.as_secs_f64()
    );
    println!(
        "  counters: chained={} ibtc_filled={} fast_hits={} misses={}",
        jv.cache.chained(),
        jv.cache.ibtc_filled(),
        jc.fast_hits(),
        jv.cache.misses()
    );
}

/// IBTC for a monomorphic indirect jump (fast-dispatch R4): a `jmp reg` back-edge whose
/// target never changes must fill its per-site slot once and then chain, not
/// re-dispatch every iteration.
#[test]
fn monomorphic_indirect_jump_fills_and_chains_via_ibtc() {
    // mov ecx,1000; lea rdx,[top]; top: sub ecx,1; jz done; jmp rdx; done: hlt
    let build = |a: &mut CodeAssembler| {
        let mut top = a.create_label();
        let mut done = a.create_label();
        a.mov(ecx, 1000i32).unwrap();
        a.lea(rdx, qword_ptr(top)).unwrap();
        a.set_label(&mut top).unwrap();
        a.sub(ecx, 1i32).unwrap();
        a.jz(done).unwrap();
        a.jmp(rdx).unwrap();
        a.set_label(&mut done).unwrap();
        a.hlt().unwrap();
    };

    let (vm, cpu) = run_flat_to_hlt(build, Box::new(JitBackend::new()));
    assert_eq!(cpu.reg(Reg::Rcx) as u32, 0, "loop ran to completion");
    // The single indirect site is monomorphic: fill the descriptor a handful of
    // times at most (ideally once), then chain every remaining iteration.
    assert!(
        vm.cache.ibtc_filled() >= 1,
        "IBTC never fired: {}",
        vm.cache.ibtc_filled()
    );
    assert!(
        vm.cache.ibtc_filled() <= 3,
        "monomorphic site refilled too often: {}",
        vm.cache.ibtc_filled()
    );
    assert!(
        vm.cache.chained() > 500,
        "indirect back-edge didn't chain: {}",
        vm.cache.chained()
    );
}

/// A polymorphic `jmp reg` (two alternating targets) must stay correct through
/// repeated IBTC refills and the megamorphic cap (R4): the target compare guards
/// every hit, and a mismatch/over-cap simply re-dispatches. Validated against the
/// interpreter.
#[test]
fn polymorphic_indirect_jump_matches_interpreter() {
    // Alternate the jmp-reg target between tA and tB each iteration; accumulate a
    // target-dependent value so a wrong transfer would corrupt eax.
    let build = |a: &mut CodeAssembler| {
        let mut loop_top = a.create_label();
        let mut ta = a.create_label();
        let mut tb = a.create_label();
        let mut next = a.create_label();
        a.mov(ecx, 200i32).unwrap();
        a.xor(eax, eax).unwrap();
        a.lea(r8, qword_ptr(ta)).unwrap();
        a.lea(r9, qword_ptr(tb)).unwrap();
        a.mov(rdx, r8).unwrap();
        a.set_label(&mut loop_top).unwrap();
        a.jmp(rdx).unwrap();
        a.set_label(&mut ta).unwrap();
        a.add(eax, 1i32).unwrap();
        a.mov(rdx, r9).unwrap(); // next target = B
        a.jmp(next).unwrap();
        a.set_label(&mut tb).unwrap();
        a.add(eax, 3i32).unwrap();
        a.mov(rdx, r8).unwrap(); // next target = A
        a.set_label(&mut next).unwrap();
        a.sub(ecx, 1i32).unwrap();
        a.jnz(loop_top).unwrap();
        a.hlt().unwrap();
    };

    let (_vj, jit) = run_flat_to_hlt(build, Box::new(JitBackend::new()));
    let (_vi, interp) = run_flat_to_hlt(build, Box::new(InterpreterBackend));
    assert_eq!(
        (jit.reg(Reg::Rax), jit.reg(Reg::Rcx)),
        (interp.reg(Reg::Rax), interp.reg(Reg::Rcx)),
        "polymorphic jmp reg diverged from the interpreter"
    );
    // 100×1 (A) + 100×3 (B) = 400.
    assert_eq!(jit.reg(Reg::Rax) as u32, 400, "alternation result");
}

/// Return prediction (fast-dispatch R5): a loop calling a leaf subroutine must chain
/// *both* the call edge (R2) and the return edge (R5), so the chained-transfer
/// count runs well past the R2-only level (call + back-edge ≈ 2/iter → ≈ 3/iter).
#[test]
fn return_prediction_chains_the_ret_edge() {
    let build = |a: &mut CodeAssembler| {
        let mut top = a.create_label();
        let mut sub = a.create_label();
        a.mov(ecx, 1000i32).unwrap();
        a.mov(eax, 0i32).unwrap();
        a.set_label(&mut top).unwrap();
        a.call(sub).unwrap();
        a.add(eax, ecx).unwrap();
        a.sub(ecx, 1i32).unwrap();
        a.jnz(top).unwrap();
        a.hlt().unwrap();
        a.set_label(&mut sub).unwrap();
        a.ret().unwrap();
    };

    let (vm, cpu) = run_flat_to_hlt(build, Box::new(JitBackend::new()));
    assert_eq!(cpu.reg(Reg::Rax) as u32, 500_500, "call/ret loop result");
    // R2 alone (call + back-edge) would give ~2000; the predicted ret adds ~1000.
    assert!(
        vm.cache.chained() > 2500,
        "return edge didn't chain: {}",
        vm.cache.chained()
    );
}

/// A mispredicted return must never follow the shadow ring (R5): the guest
/// overwrites its return address on the stack, so the actual popped target differs
/// from the prediction. The addr compare must reject the prediction and dispatch
/// to the real target. Validated for exact control flow and against the interpreter.
#[test]
fn overwritten_return_address_is_not_mispredicted() {
    // call sub; (predicted return: mov ebx,111) ; sub rewrites [rsp] to target_b,
    // so ret lands on target_b (ebx=222) instead.
    let build = |a: &mut CodeAssembler| {
        let mut sub = a.create_label();
        let mut target_b = a.create_label();
        let mut end = a.create_label();
        a.call(sub).unwrap();
        a.mov(ebx, 111i32).unwrap(); // predicted continuation — must be skipped
        a.jmp(end).unwrap();
        a.set_label(&mut target_b).unwrap();
        a.mov(ebx, 222i32).unwrap(); // real target after the stack rewrite
        a.set_label(&mut end).unwrap();
        a.hlt().unwrap();
        a.set_label(&mut sub).unwrap();
        a.lea(rax, qword_ptr(target_b)).unwrap();
        a.mov(qword_ptr(rsp), rax).unwrap(); // clobber the return address
        a.ret().unwrap();
    };

    let (_vj, jit) = run_flat_to_hlt(build, Box::new(JitBackend::new()));
    let (_vi, interp) = run_flat_to_hlt(build, Box::new(InterpreterBackend));
    assert_eq!(
        jit.reg(Reg::Rbx) as u32,
        222,
        "ret must honor the rewritten stack"
    );
    assert_eq!(
        jit.reg(Reg::Rbx),
        interp.reg(Reg::Rbx),
        "JIT and interpreter must agree on the mispredicted return"
    );
}

/// Recursion deeper than the shadow ring (64 frames) must stay correct (R5): frames
/// beyond the ring wrap and overwrite older ones, so the deepest returns mispredict
/// and fall back to dispatch — never a wrong transfer. Sum 1..=100 via recursion.
#[test]
fn deep_recursion_beyond_ring_wraps_correctly() {
    let build = |a: &mut CodeAssembler| {
        let mut rec = a.create_label();
        let mut done = a.create_label();
        a.mov(ecx, 100i32).unwrap();
        a.xor(eax, eax).unwrap();
        a.call(rec).unwrap();
        a.hlt().unwrap();
        // rec: if ecx==0 ret; else acc += ecx; ecx -= 1; call rec; ret
        a.set_label(&mut rec).unwrap();
        a.test(ecx, ecx).unwrap();
        a.jz(done).unwrap();
        a.add(eax, ecx).unwrap();
        a.dec(ecx).unwrap();
        a.call(rec).unwrap();
        a.set_label(&mut done).unwrap();
        a.ret().unwrap();
    };

    let (_vj, jit) = run_flat_to_hlt(build, Box::new(JitBackend::new()));
    let (_vi, interp) = run_flat_to_hlt(build, Box::new(InterpreterBackend));
    assert_eq!(jit.reg(Reg::Rax) as u32, 5050, "recursive sum 1..=100");
    assert_eq!(
        (jit.reg(Reg::Rax), jit.reg(Reg::Rcx)),
        (interp.reg(Reg::Rax), interp.reg(Reg::Rcx)),
        "deep recursion diverged from the interpreter"
    );
}

/// fxsave/fxrstor round-trip (glibc's PLT resolver uses them to preserve XMM):
/// load a known value into an XMM reg, fxsave the FP state, clobber the reg,
/// fxrstor, and the reg must come back. Interp and JIT must agree (the shared
/// exec_fxstate routine), also validated against native by the busybox:glibc tests.
#[test]
fn fxsave_fxrstor_round_trips_xmm() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm3, xmmword_ptr(SCRATCH)).unwrap();
            a.fxsave(xmmword_ptr(SCRATCH + 64)).unwrap(); // 512-byte save area
            a.pxor(xmm3, xmm3).unwrap(); // clobber
            a.fxrstor(xmmword_ptr(SCRATCH + 64)).unwrap(); // restore → xmm3 back
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// Hotness-gated tier-up (FD tiering): a block starts interpreted and is JIT-
/// compiled only after it runs `tier_up_after` times. The tiered run must produce
/// the identical result to eager compilation, and a hot loop must actually tier up
/// (its back-edge chains once compiled — proof the JIT engaged).
#[test]
fn tiering_matches_eager_and_tiers_up() {
    let build = |a: &mut CodeAssembler| {
        let mut top = a.create_label();
        a.mov(ecx, 5000i32).unwrap();
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

    let run = |tier: Option<u32>| {
        let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(JitBackend::new()));
        vm.set_tier_up_after(tier);
        vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        assert!(matches!(cpu.run(&vm, Some(1_000_000)), Exit::Hlt));
        (cpu.reg(Reg::Rax), vm.cache.chained())
    };

    let (eager_rax, _) = run(None);
    let (tier_rax, tier_chained) = run(Some(10));
    assert_eq!(tier_rax, eager_rax, "tiered result must match eager");
    assert!(
        tier_chained > 100,
        "hot loop never tiered up to the chaining JIT: chained={tier_chained}"
    );
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
        cpu_init: CpuSnapshot {
            rip: CODE,
            ..Default::default()
        },
        mem_init: vec![MemChunk {
            addr: CODE,
            bytes: code,
            kind: MemKind::Ram,
        }],
        entry: CODE,
        run: RunSpec::Blocks(u64::MAX),
    };

    let t0 = std::time::Instant::now();
    let i = run_with_backend(&input, Box::new(InterpreterBackend));
    let interp_ms = t0.elapsed().as_secs_f64() * 1e3;

    let t1 = std::time::Instant::now();
    let j = run_with_backend(&input, Box::new(JitBackend::new()));
    let jit_ms = t1.elapsed().as_secs_f64() * 1e3;

    assert!(
        compare(&i, &j, &[]).is_none(),
        "JIT result must match interpreter"
    );
    eprintln!(
        "loop of {n} iters: interp {interp_ms:.1} ms, jit {jit_ms:.1} ms, speedup {:.1}x",
        interp_ms / jit_ms
    );
    assert!(
        jit_ms < interp_ms,
        "JIT should beat the interpreter on a hot loop"
    );
}

fn collect_ron(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_ron(&path, out);
        } else if path.extension().is_some_and(|e| e == "ron") {
            out.push(path);
        }
    }
}

/// AVX-256 data movement (task-168.2): the JIT must handle 256-bit `vmovdqu`
/// (memory round-trip) and reg-reg `vmovdqa` on YMM identically to the interpreter,
/// including the upper 128-bit halves (`compare` checks `ymm_hi`).
#[test]
fn avx256_vmovdqu_and_vmov_match_interp() {
    const LO: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const HI: u128 = 0xFF00_FF00_1234_5678_9ABC_DEF0_0011_2233;
    jit_eq_interp(
        |a| {
            a.vmovdqu(ymmword_ptr(SCRATCH), ymm0).unwrap(); // store 32 bytes
            a.vmovdqu(ymm1, ymmword_ptr(SCRATCH)).unwrap(); // load back
            a.vmovdqa(ymm2, ymm0).unwrap(); // reg-reg 256-bit copy
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = LO;
            c.ymm_hi[0] = HI;
        },
        &[],
    );
}

/// AVX-256 logic / packed arithmetic / movemask (task-168.2): register and 32-byte
/// memory-source forms, plus the 32-bit `vpmovmskb` on a YMM — JIT must match the
/// interpreter on both halves.
#[test]
fn avx256_logic_packed_movemask_match_interp() {
    const LO: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const HI: u128 = 0xFF00_FF00_1234_5678_9ABC_DEF0_0011_2233;
    jit_eq_interp(
        |a| {
            a.vpxor(ymm3, ymm0, ymm1).unwrap();
            a.vpand(ymm4, ymm0, ymm1).unwrap();
            a.vpcmpeqb(ymm5, ymm0, ymm1).unwrap();
            a.vpsubb(ymm6, ymm0, ymm1).unwrap();
            a.vpmovmskb(eax, ymm5).unwrap(); // 32-bit mask across 32 bytes
            a.vmovdqu(ymmword_ptr(SCRATCH), ymm1).unwrap(); // seed a 32-byte source
            a.vpor(ymm7, ymm0, ymmword_ptr(SCRATCH)).unwrap(); // 256 logic, mem source
            a.vpaddb(ymm8, ymm0, ymmword_ptr(SCRATCH)).unwrap(); // 256 packed, mem source
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = LO;
            c.ymm_hi[0] = HI;
            c.xmm[1] = HI;
            c.ymm_hi[1] = LO;
        },
        &[],
    );
}

/// AVX2 broadcast / 128-lane insert+extract (task-168.3): register and memory-source
/// vpbroadcast (128 and 256 dests), vinserti128, vextracti128 — JIT == interp.
#[test]
fn avx2_broadcast_insert_extract_match_interp() {
    const LO: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const HI: u128 = 0xFF00_FF00_1234_5678_9ABC_DEF0_0011_2233;
    const INS: u128 = 0xAAAA_BBBB_CCCC_DDDD_1111_2222_3333_4444;
    jit_eq_interp(
        |a| {
            a.vpbroadcastb(ymm1, xmm0).unwrap(); // byte -> full YMM
            a.vpbroadcastd(xmm2, xmm0).unwrap(); // dword -> XMM (upper zeroed)
            a.vpbroadcastq(ymm3, xmm0).unwrap();
            a.vinserti128(ymm4, ymm0, xmm5, 1).unwrap(); // insert into the high lane
            a.vextracti128(xmm6, ymm0, 1).unwrap(); // extract the high lane
            a.vmovdqu(ymmword_ptr(SCRATCH), ymm0).unwrap(); // seed a source
            a.vpbroadcastw(ymm7, word_ptr(SCRATCH)).unwrap(); // memory-source broadcast
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = LO;
            c.ymm_hi[0] = HI;
            c.xmm[5] = INS;
        },
        &[],
    );
}

/// AVX2 256-bit vpshufb (per-lane) + VEX shift-by-immediate, 256 and 128 (task-168.3).
#[test]
fn avx256_shift_and_shuffle_match_interp() {
    const LO: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const HI: u128 = 0xFF00_FF00_1234_5678_9ABC_DEF0_0011_2233;
    // Shuffle indices: some in-lane byte positions, some with the high bit (zeroing).
    const IDX: u128 = 0x8000_0102_0304_0506_0708_090A_0B0C_0D0E;
    jit_eq_interp(
        |a| {
            a.vpshufb(ymm2, ymm0, ymm1).unwrap(); // per-128-lane shuffle across 256
            a.vpsllw(ymm3, ymm0, 3i32).unwrap();
            a.vpsrld(ymm4, ymm0, 5i32).unwrap();
            a.vpsraw(ymm5, ymm0, 2i32).unwrap();
            a.vpslld(xmm6, xmm0, 4i32).unwrap(); // 128-bit VEX shift (zeroes upper)
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = LO;
            c.ymm_hi[0] = HI;
            c.xmm[1] = IDX;
            c.ymm_hi[1] = IDX;
        },
        &[],
    );
}

/// AVX2 cross-lane permutes (task-168.3): vpermq (imm), vpermd (reg control),
/// vperm2i128 (lane select + zero), vpalignr 256 and VEX.128 — JIT == interp.
#[test]
fn avx2_cross_lane_permutes_match_interp() {
    const LO: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const HI: u128 = 0xFF00_FF00_1234_5678_9ABC_DEF0_0011_2233;
    // vpermd control: one dword index per lane (only low 3 bits matter).
    const CTRL: u128 = 0x0000_0007_0000_0003_0000_0005_0000_0001;
    jit_eq_interp(
        |a| {
            a.vpermq(ymm2, ymm0, 0b_00_01_10_11i32).unwrap(); // reverse the 4 qwords
            a.vpermd(ymm3, ymm1, ymm0).unwrap(); // gather dwords by ymm1 control
            a.vperm2i128(ymm4, ymm0, ymm1, 0x31i32).unwrap(); // hi<-b.hi, lo<-a.hi
            a.vperm2i128(ymm5, ymm0, ymm1, 0x08i32).unwrap(); // lo lane zeroed
            a.vpalignr(ymm6, ymm0, ymm1, 5i32).unwrap(); // per-lane byte align
            a.vpalignr(xmm7, xmm0, xmm1, 3i32).unwrap(); // VEX.128 (zeroes upper)
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = LO;
            c.ymm_hi[0] = HI;
            c.xmm[1] = CTRL;
            c.ymm_hi[1] = HI;
        },
        &[],
    );
}

/// CPU feature selection (task-169): the guest's `cpuid`/`xgetbv` observe the
/// embedder-chosen feature set, identically on interp and JIT. Default advertises no
/// AVX-512 (leaf-7 EBX bit 16 clear, XCR0=0x7); `v4` advertises it (bit 16 set,
/// XCR0=0xE7).
#[test]
fn cpu_features_drive_cpuid_and_xgetbv() {
    let snippet = |a: &mut CodeAssembler| {
        a.mov(eax, 7i32).unwrap();
        a.xor(ecx, ecx).unwrap();
        a.cpuid().unwrap();
        a.mov(dword_ptr(SCRATCH), ebx).unwrap(); // leaf-7 EBX
        a.mov(eax, 0i32).unwrap();
        a.xor(ecx, ecx).unwrap();
        a.xgetbv().unwrap();
        a.mov(dword_ptr(SCRATCH + 8), eax).unwrap(); // XCR0
        a.hlt().unwrap();
    };
    let run = |features: GuestCpuFeatures, backend: Box<dyn x86jit_core::Backend>| -> (u32, u32) {
        let mut asm = CodeAssembler::new(64).unwrap();
        snippet(&mut asm);
        let code = asm.assemble(CODE).unwrap();
        let input = VectorInput {
            cpu_init: CpuSnapshot {
                rip: CODE,
                ..Default::default()
            },
            mem_init: vec![
                MemChunk {
                    addr: CODE,
                    bytes: code,
                    kind: MemKind::Ram,
                },
                MemChunk {
                    addr: SCRATCH,
                    bytes: vec![0u8; SCRATCH_LEN],
                    kind: MemKind::Ram,
                },
            ],
            entry: CODE,
            run: RunSpec::UntilExit,
        };
        let out = run_with_backend_features(&input, backend, features);
        let scratch = out.mem.iter().find(|c| c.addr == SCRATCH).unwrap();
        let leaf7 = u32::from_le_bytes(scratch.bytes[0..4].try_into().unwrap());
        let xcr0v = u32::from_le_bytes(scratch.bytes[8..12].try_into().unwrap());
        (leaf7, xcr0v)
    };

    for (feat, avx512, xcr0) in [
        (GuestCpuFeatures::default(), false, 0x7u32),
        (GuestCpuFeatures::v4(), true, 0xE7u32),
    ] {
        let i = run(feat, Box::new(InterpreterBackend));
        let j = run(feat, Box::new(JitBackend::new()));
        assert_eq!(i, j, "interp and JIT must observe the same features");
        assert_eq!((i.0 & (1 << 16)) != 0, avx512, "AVX512F bit for {feat:?}");
        assert_eq!(i.1, xcr0, "XCR0 for {feat:?}");
    }
}

/// BMI2 pdep/pext (flagless bit gather/scatter) + mulx (flagless widening multiply,
/// task-168.5.3) — JIT == interp; both flagless, so flags stay put.
#[test]
fn pdep_pext_mulx_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x1234_5678_9ABC_DEF0u64).unwrap();
            a.mov(rbx, 0x0F0F_0F0F_0F0F_0F0Fu64).unwrap(); // mask
            a.pdep(rcx, rax, rbx).unwrap();
            a.pext(rsi, rax, rbx).unwrap();
            a.mov(rdx, 0x1_0000_0003u64).unwrap(); // mulx's implicit multiplier
            a.mulx(r8, r9, rax).unwrap(); // r8=hi, r9=lo = rdx*rax
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// BMI2 flagless shifts/rotate (task-168.5.3): shlx/shrx/sarx/rorx reuse the existing
/// shift/rotate IR with FlagMask::NONE — same result, flags untouched — JIT == interp.
#[test]
fn bmi_flagless_shifts_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0xF234_5678_9ABC_DEF0u64).unwrap();
            a.mov(rcx, 12u64).unwrap();
            a.shlx(rbx, rax, rcx).unwrap();
            a.shrx(rdx, rax, rcx).unwrap();
            a.sarx(rsi, rax, rcx).unwrap(); // arithmetic: sign fill
            a.rorx(rdi, rax, 20u32).unwrap(); // imm8 count
            a.shlx(r8d, eax, ecx).unwrap(); // 32-bit
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// BMI1/BMI2 family (task-168.5.3): andn/blsi/blsr/blsmsk/bextr/bzhi — the JIT's bmi
/// helper path (stack-slot result+CF, flag extraction) matches interp. Semantics are
/// pinned separately by the bmi_result unit test.
#[test]
fn bmi_family_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x0F0F_0F0Cu64).unwrap();
            a.mov(rbx, 0xFF00_FF00u64).unwrap();
            a.andn(rcx, rax, rbx).unwrap();
            a.blsi(rdx, rax).unwrap();
            a.blsr(rsi, rax).unwrap();
            a.blsmsk(rdi, rax).unwrap();
            a.mov(r8, 4u64 | (8u64 << 8)).unwrap();
            a.bextr(r9, rax, r8).unwrap();
            a.mov(r10, 8u64).unwrap();
            a.bzhi(r11, rax, r10).unwrap();
            a.andn(r12d, eax, ebx).unwrap(); // 32-bit form (size seam)
            a.mov(r13, 0u64).unwrap();
            a.blsr(r14, r13).unwrap(); // zero-source: CF=1, ZF=1
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// v3 scalars tzcnt/lzcnt/movbe (task-176): counts defined on a zero source (=
/// bit-width) with CF/ZF, and byte-swapped load/store — JIT == interp.
#[test]
fn tzcnt_lzcnt_movbe_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x0000_0000_00FF_0000u64).unwrap(); // 16 trailing zeros
            a.tzcnt(rbx, rax).unwrap();
            a.lzcnt(rcx, rax).unwrap();
            a.tzcnt(esi, eax).unwrap(); // 32-bit form
            a.mov(rdx, 0u64).unwrap();
            a.tzcnt(rdi, rdx).unwrap(); // zero -> 64, CF=1, ZF=0
            a.lzcnt(r8, rdx).unwrap(); // zero -> 64
                                       // movbe round-trip: store rax, byteswap-load into r9, byteswap-store back.
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.movbe(r9, qword_ptr(SCRATCH)).unwrap();
            a.movbe(qword_ptr(SCRATCH + 8), r9).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// Host codegen target (task-175): a JIT pinned to `HostTarget::Baseline` (no AVX)
/// must still execute a guest AVX2 op correctly — Cranelift lowers the 256-bit lanes
/// to SSE, so interp == baseline-JIT. Proves the host-codegen axis is orthogonal to
/// the guest ISA and stays guest-invisible.
#[test]
fn baseline_host_target_lowers_guest_avx_to_sse() {
    const A: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const B: u128 = 0x1010_1010_1010_1010_2020_2020_2020_2020;
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.vpaddb(ymm2, ymm0, ymm1).unwrap(); // AVX2 256-bit packed byte add
    asm.vmovdqu(ymmword_ptr(SCRATCH), ymm2).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();
    let mut cpu = CpuSnapshot {
        rip: CODE,
        ..Default::default()
    };
    cpu.xmm[0] = A;
    cpu.ymm_hi[0] = B;
    cpu.xmm[1] = B;
    cpu.ymm_hi[1] = A;
    let input = VectorInput {
        cpu_init: cpu,
        mem_init: vec![
            MemChunk {
                addr: CODE,
                bytes: code,
                kind: MemKind::Ram,
            },
            MemChunk {
                addr: SCRATCH,
                bytes: vec![0u8; SCRATCH_LEN],
                kind: MemKind::Ram,
            },
        ],
        entry: CODE,
        run: RunSpec::UntilExit,
    };
    let f = GuestCpuFeatures::default();
    let interp = run_with_backend_features(&input, Box::new(InterpreterBackend), f);
    let jit = run_with_backend_features(
        &input,
        Box::new(JitBackend::with_host_target(HostTarget::Baseline)),
        f,
    );
    assert!(
        compare(&interp, &jit, &[]).is_none(),
        "baseline-pinned JIT diverged from interp:\n{}",
        compare(&interp, &jit, &[]).unwrap()
    );
}

/// AVX-512 write-masking (task-170.1): masked `vmovdqu32 xmm{k1}` merge + `{k1}{z}`
/// zero. Asserts the exact blended bytes (correctness of the shared write_masked), and
/// that interp == JIT. k1 = 0b0101 → dword lanes 0,2 written, 1,3 merged/zeroed.
#[test]
fn avx512_masked_vmovdqu32_merge_and_zero() {
    let build = |a: &mut CodeAssembler| {
        a.mov(dword_ptr(SCRATCH), 0x1111_1111u32 as i32).unwrap();
        a.mov(dword_ptr(SCRATCH + 4), 0x2222_2222u32 as i32)
            .unwrap();
        a.mov(dword_ptr(SCRATCH + 8), 0x3333_3333u32 as i32)
            .unwrap();
        a.mov(dword_ptr(SCRATCH + 12), 0x4444_4444u32 as i32)
            .unwrap();
        a.mov(eax, 0xAAAA_AAAAu32 as i32).unwrap();
        for off in [16u64, 20, 24, 28] {
            a.mov(dword_ptr(SCRATCH + off), eax).unwrap();
        }
        a.mov(eax, 0b0101i32).unwrap();
        a.kmovw(k1, eax).unwrap();
        a.vmovdqu32(xmm0, xmmword_ptr(SCRATCH)).unwrap(); // src
        a.vmovdqu32(xmm1, xmmword_ptr(SCRATCH + 16)).unwrap(); // merge base
        a.vmovdqu32(xmm1.k1(), xmm0).unwrap(); // masked merge
        a.vmovdqu32(xmmword_ptr(SCRATCH + 32), xmm1).unwrap();
        a.vmovdqu32(xmm2, xmmword_ptr(SCRATCH + 16)).unwrap();
        a.vmovdqu32(xmm2.k1().z(), xmm0).unwrap(); // masked zero
        a.vmovdqu32(xmmword_ptr(SCRATCH + 48), xmm2).unwrap();
        a.hlt().unwrap();
    };
    // interp == JIT (both route the masked op through the same write_masked helper).
    jit_eq_interp_features(GuestCpuFeatures::v4(), build, |_| {}, &[]);

    // Absolute correctness: run interp, check the blended bytes.
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();
    let input = VectorInput {
        cpu_init: CpuSnapshot {
            rip: CODE,
            ..Default::default()
        },
        mem_init: vec![
            MemChunk {
                addr: CODE,
                bytes: code,
                kind: MemKind::Ram,
            },
            MemChunk {
                addr: SCRATCH,
                bytes: vec![0u8; SCRATCH_LEN],
                kind: MemKind::Ram,
            },
        ],
        entry: CODE,
        run: RunSpec::UntilExit,
    };
    let out =
        run_with_backend_features(&input, Box::new(InterpreterBackend), GuestCpuFeatures::v4());
    let s = out.mem.iter().find(|c| c.addr == SCRATCH).unwrap();
    let dw = |off: usize| u32::from_le_bytes(s.bytes[off..off + 4].try_into().unwrap());
    // merge: lanes 0,2 from src; 1,3 kept from the 0xAAAAAAAA base.
    assert_eq!(
        [dw(32), dw(36), dw(40), dw(44)],
        [0x1111_1111, 0xAAAA_AAAA, 0x3333_3333, 0xAAAA_AAAA]
    );
    // zero: lanes 0,2 from src; 1,3 zeroed.
    assert_eq!(
        [dw(48), dw(52), dw(56), dw(60)],
        [0x1111_1111, 0, 0x3333_3333, 0]
    );
}

/// AVX-512 foundation (task-168.5): unmasked 512-bit `vmovdqu64` load, `vmovdqa64`
/// register move, and store round-trip all four ZMM lanes — JIT == interp. Seeds 8
/// distinct qwords, moves them through a ZMM, and compares the stored result memory.
#[test]
fn avx512_vmovdqu512_load_mov_store_match_interp() {
    jit_eq_interp(
        |a| {
            for i in 0..8u64 {
                let v = 0xDEAD_0000_0000_0000u64 | (0x1111_1111_1111u64.wrapping_mul(i + 1));
                a.mov(rax, v).unwrap();
                a.mov(qword_ptr(SCRATCH + i * 8), rax).unwrap();
            }
            a.vmovdqu64(zmm1, zmmword_ptr(SCRATCH)).unwrap(); // 512-bit load
            a.vmovdqa64(zmm3, zmm1).unwrap(); // reg-reg 512-bit move
            a.vmovdqu64(zmmword_ptr(SCRATCH + 0x80), zmm3).unwrap(); // 512-bit store
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// AVX-512 grind (task-168.5): EVEX/VEX scalar-ish ops that CachyOS v4 binaries
/// hit — vpinsrq/vpextrq (VEX lane in/out), vpmaxuq (EVEX 64-bit unsigned max),
/// and vpbroadcastd from a GPR — all JIT == interp.
#[test]
fn avx512_evex_scalar_ops_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0xDEAD_BEEF_1234_5678u64).unwrap();
            a.vpinsrq(xmm1, xmm0, rax, 1).unwrap(); // insert into qword lane 1
            a.vpextrq(rbx, xmm1, 1).unwrap(); // rbx == rax
            a.vpmaxuq(xmm4, xmm2, xmm3).unwrap(); // unsigned 64-bit max per lane
            a.mov(ecx, 0x0A0B_0C0Du32 as i32).unwrap();
            a.vpbroadcastd(xmm5, ecx).unwrap(); // broadcast GPR dword → xmm
            a.hlt().unwrap();
        },
        |c| {
            // lane0 low, lane1 high. High bit set → distinguishes unsigned from signed.
            c.xmm[2] = 0x0000_0000_0000_0001 | (0x8000_0000_0000_0000u128 << 64);
            c.xmm[3] = 0xFFFF_FFFF_FFFF_FFFF | (0x0000_0000_0000_0002u128 << 64);
        },
        &[],
    );
}

/// AVX-512 opmask moves (task-168.5): kmov{b,w,d,q} between GPR, opmask, opmask,
/// and memory — width truncation and round-trips all JIT == interp.
#[test]
fn avx512_kmov_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(eax, 0xABCD_1234u32 as i32).unwrap();
            a.kmovd(k1, eax).unwrap(); // GPR → k
            a.kmovd(ebx, k1).unwrap(); // k → GPR (rbx = 0xABCD1234)
            a.kmovw(k2, k1).unwrap(); // k → k (16-bit)
            a.kmovw(ecx, k2).unwrap(); // rcx = 0x1234
            a.kmovb(k3, eax).unwrap(); // GPR → k (8-bit)
            a.kmovb(edx, k3).unwrap(); // rdx = 0x34
            a.kmovd(dword_ptr(SCRATCH), k1).unwrap(); // k → mem
            a.kmovd(k4, dword_ptr(SCRATCH)).unwrap(); // mem → k
            a.kmovd(esi, k4).unwrap(); // rsi = 0xABCD1234
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// AVX-512 opmask subsystem (task-168.5): mask-producing compare `vpcmpb` → k and
/// the `kortest` flag test that consumes it. Captures ZF/CF for an all-equal mask
/// (→ CF=1) and a partially-equal mask (→ CF=0, ZF=0) — JIT == interp.
#[test]
fn avx512_vpcmp_kortest_match_interp() {
    const A: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    // B differs from A in some bytes → EQ mask has holes; all-equal case uses A vs A.
    const B: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_01FF;
    jit_eq_interp(
        |a| {
            a.vpcmpb(k1, xmm0, xmm1, 0).unwrap(); // EQ, 16 byte lanes → k1
            a.kortestw(k1, k1).unwrap(); // ZF=(k1==0), CF=(k1==0xFFFF)
            a.setz(r8b).unwrap();
            a.setb(r9b).unwrap();
            a.vpcmpb(k2, xmm0, xmm0, 0).unwrap(); // all equal → mask = 0xFFFF
            a.kortestw(k2, k2).unwrap();
            a.setz(r10b).unwrap();
            a.setb(r11b).unwrap(); // expect CF=1
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.xmm[1] = B;
        },
        &[],
    );
}

/// AVX-512 `vptestm`/`vptestnm` → opmask (task-168.5.4): `k[i] = (a & b) != 0` (or `== 0`
/// for the `nm` "not-mask" form glibc's strlen uses to find zero bytes). Byte and dword
/// lanes, 128- and 256-bit, mask moved to a GPR so the opmask result is compared —
/// JIT == interp.
#[test]
fn avx512_vptest_to_mask_match_interp() {
    // Byte 2 of xmm1 is zero, so vptestnmb marks it; xmm0/xmm1 share some bits.
    const A: u128 = 0xFF01_8040_0102_0408_1020_4080_00FF_0F0F;
    const B: u128 = 0x0F0F_0F0F_0F0F_0F0F_0F0F_0F0F_0F0F_0F0F;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vptestmb(k1, xmm0, xmm1).unwrap(); // (a&b)!=0 per byte
            a.kmovd(r8d, k1).unwrap();
            a.vptestnmb(k2, xmm0, xmm1).unwrap(); // (a&b)==0 per byte
            a.kmovd(r9d, k2).unwrap();
            a.vptestmd(k3, xmm0, xmm1).unwrap(); // dword lanes
            a.kmovd(r10d, k3).unwrap();
            a.vptestnmb(k4, ymm2, ymm3).unwrap(); // 256-bit → 32 mask bits
            a.kmovd(r11d, k4).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.xmm[1] = B;
            c.xmm[2] = A;
            c.ymm_hi[2] = B;
            c.xmm[3] = B;
            c.ymm_hi[3] = A;
        },
        &[],
    );
}

/// AVX-512 dedicated-opcode masked compares (task-168.5.1): the EVEX forms of
/// `vpcmpeq{b,d}` / `vpcmpgt{b,d}` write an opmask `k` (glibc's heaviest AVX-512 op).
/// Each mask is moved to a GPR with `kmovd` so the *opmask result itself* — not just
/// vector state — is compared JIT == interp, across 128- and 256-bit forms and a
/// write-masked variant.
#[test]
fn avx512_vpcmpeq_gt_to_mask_match_interp() {
    // A vs B: byte lanes 0 and 15 differ; signed byte 15 is 0x80 (< everything).
    const A: u128 = 0x000E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const B: u128 = 0x800E_0D0C_0B0A_0908_0706_0504_0302_01FF;
    const HI_A: u128 = 0x1111_1111_1111_1111_2222_2222_2222_2222;
    const HI_B: u128 = 0x1111_1111_1111_1111_2222_2222_2222_3333;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // EQ, byte lanes → k1; move the mask to a GPR to compare it directly.
            a.vpcmpeqb(k1, xmm0, xmm1).unwrap();
            a.kmovd(r8d, k1).unwrap();
            // signed GT, byte lanes → k2.
            a.vpcmpgtb(k2, xmm0, xmm1).unwrap();
            a.kmovd(r9d, k2).unwrap();
            // EQ, dword lanes → k3 (4 lanes over 128 bits).
            a.vpcmpeqd(k3, xmm0, xmm1).unwrap();
            a.kmovd(r10d, k3).unwrap();
            // signed GT, dword lanes → k4.
            a.vpcmpgtd(k4, xmm0, xmm1).unwrap();
            a.kmovd(r11d, k4).unwrap();
            // 256-bit form: EQ byte lanes over ymm → k5 (32 mask bits).
            a.vpcmpeqb(k5, ymm2, ymm3).unwrap();
            a.kmovd(r12d, k5).unwrap();
            // Write-masked: k7 restricts which lanes compare; equal inputs give all-ones
            // ANDed with k7, so the result must equal k7's low 16 bits.
            a.mov(eax, 0x5A5Ai32).unwrap();
            a.kmovw(k7, eax).unwrap();
            a.vpcmpeqb(k6.k7(), xmm0, xmm0).unwrap();
            a.kmovd(r13d, k6).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.xmm[1] = B;
            c.xmm[2] = A;
            c.ymm_hi[2] = HI_A;
            c.xmm[3] = B;
            c.ymm_hi[3] = HI_B;
        },
        &[],
    );
}

/// AVX-512 EVEX lane ops (task-168.5.6): `vinserti32x4`/`64x2` (128-bit lane insert),
/// `vinserti64x4` (256-bit half insert) and `valignd`/`valignq` (cross-512 element
/// shift), each crossing a lane boundary — JIT == interp (ZMM state via task-193).
#[test]
fn avx512_lane_ops_match_interp() {
    // Fill all four 128-bit lanes of ZMM `r` with a register-distinct pattern.
    fn seed(c: &mut CpuSnapshot, r: usize, tag: u128) {
        c.xmm[r] = tag ^ 0x1111_1111_1111_1111_1111_1111_1111_1111;
        c.ymm_hi[r] = tag ^ 0x2222_2222_2222_2222_2222_2222_2222_2222;
        c.zmm_hi[r][0] = tag ^ 0x3333_3333_3333_3333_3333_3333_3333_3333;
        c.zmm_hi[r][1] = tag ^ 0x4444_4444_4444_4444_4444_4444_4444_4444;
    }
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vinserti32x4(zmm0, zmm1, xmm2, 2).unwrap(); // 128-bit into lane 2
            a.vinserti64x2(zmm3, zmm4, xmm5, 3).unwrap(); // 128-bit into lane 3
            a.vinserti64x4(zmm6, zmm7, ymm8, 1).unwrap(); // 256-bit into the high half
            a.valignd(zmm9, zmm10, zmm11, 3).unwrap(); // shift right 3 dwords
            a.valignq(zmm12, zmm13, zmm14, 5).unwrap(); // shift right 5 qwords
            a.hlt().unwrap();
        },
        |c| {
            // Seed every source register with a distinct pattern (its index as the tag).
            for r in [1usize, 2, 4, 5, 7, 8, 10, 11, 13, 14] {
                seed(c, r, r as u128);
            }
        },
        &[],
    );
}

/// AVX-512 masked EVEX logic (task-168.5.5): `vpxor/vpand/vpor{d,q}` with a write-mask,
/// covering merge (keep dst) vs zero `{z}`, and the all-ones / all-zero mask edges, at
/// 128- and 256-bit widths — JIT == interp (both route through `write_masked`).
#[test]
fn avx512_masked_logic_match_interp() {
    const A: u128 = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
    const B: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    const D: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10; // merge-dst seed
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010i32).unwrap();
            a.kmovw(k1, eax).unwrap(); // partial mask
            a.mov(eax, 0xFFFFi32).unwrap();
            a.kmovw(k2, eax).unwrap(); // all-ones
            a.xor(eax, eax).unwrap();
            a.kmovw(k3, eax).unwrap(); // all-zero
            a.vpxord(xmm0.k1(), xmm1, xmm2).unwrap(); // merge, dword granularity
            a.vpxorq(xmm3.k1().z(), xmm1, xmm2).unwrap(); // zero, qword granularity
            a.vpandd(xmm4.k2(), xmm1, xmm2).unwrap(); // all-ones merge = full write
            a.vpord(xmm5.k3().z(), xmm1, xmm2).unwrap(); // all-zero zeroing = zeroed
            a.vpord(xmm6.k3(), xmm1, xmm2).unwrap(); // all-zero merge = dst unchanged
            a.vpxord(ymm7.k1(), ymm1, ymm2).unwrap(); // 256-bit masked
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.xmm[2] = B;
            c.ymm_hi[1] = B;
            c.ymm_hi[2] = A;
            for r in [0, 3, 4, 5, 6, 7] {
                c.xmm[r] = D;
            }
            c.ymm_hi[7] = D;
        },
        &[],
    );
}

/// AVX-512 512-bit logic now observable via the ZMM snapshot (task-193): `vpxorq`/
/// `vpternlogq` on full ZMM registers (upper 256 bits seeded through `zmm_hi`/`ymm_hi`)
/// — JIT == interp across all four 128-bit lanes, including bits 511:256.
#[test]
fn avx512_zmm_logic_observable_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpxorq(zmm0, zmm1, zmm2).unwrap(); // 512-bit xor
            a.vpternlogq(zmm3, zmm1, zmm2, 0x96).unwrap(); // zmm3 = zmm3 ^ zmm1 ^ zmm2
            a.hlt().unwrap();
        },
        |c| {
            // Seed all four 128-bit lanes of zmm1/zmm2/zmm3 (xmm + ymm_hi + zmm_hi).
            for (r, base) in [(1usize, 0x11u128), (2, 0x22), (3, 0x33)] {
                c.xmm[r] = base * 0x0101_0101_0101_0101_0101_0101_0101_0101;
                c.ymm_hi[r] = (base + 1) * 0x0101_0101_0101_0101_0101_0101_0101_0101;
                c.zmm_hi[r][0] = (base + 2) * 0x0101_0101_0101_0101_0101_0101_0101_0101;
                c.zmm_hi[r][1] = (base + 3) * 0x0101_0101_0101_0101_0101_0101_0101_0101;
            }
        },
        &[],
    );
}

/// SSE4.2 `pcmpistri`/`pcmpestri` (task-168.5.4): the string-aggregation index (ECX) and
/// flags across a few aggregation modes — JIT == interp (both route through the shared
/// pcmpstr helper; correctness vs hardware is covered by the native fuzz test). setcc
/// captures CF/ZF/SF/OF into GPRs so the flag path is compared too.
#[test]
fn sse42_pcmpstr_match_interp() {
    const S1: u128 = 0x00_00_6F_6C_6C_65_48_64_6C_72_6F_77_20_6F_6C_6C; // mixed bytes + nulls
    const S2: u128 = 0x00_00_00_00_00_00_00_00_6C_72_6F_77_20_6F_6C_6C;
    jit_eq_interp(
        |a| {
            a.pcmpistri(xmm0, xmm1, 0x0C).unwrap(); // equal-ordered, unsigned bytes (substring)
            a.setb(r8b).unwrap();
            a.sete(r9b).unwrap();
            a.pcmpistri(xmm0, xmm1, 0x18).unwrap(); // equal-each
            a.setb(r10b).unwrap();
            a.pcmpistri(xmm0, xmm1, 0x40).unwrap(); // equal-any, MSB index
            a.sets(r11b).unwrap();
            a.mov(eax, 6).unwrap();
            a.mov(edx, 8).unwrap();
            a.pcmpestri(xmm0, xmm1, 0x0C).unwrap(); // explicit lengths
            a.seto(r12b).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = S1;
            c.xmm[1] = S2;
        },
        &[],
    );
}

/// SSE4.1 variable blend + `round` (task-168.5.4): `blendvps/blendvpd/pblendvb` select
/// lanes by XMM0's per-lane sign bit; `round{ps,pd,ss,sd}` cover all four imm8 rounding
/// modes on values with .5 fractions (so each mode differs) — JIT == interp.
#[test]
fn sse41_blendv_round_match_interp() {
    let f32x4 = |a: f32, b: f32, c: f32, d: f32| {
        (a.to_bits() as u128)
            | ((b.to_bits() as u128) << 32)
            | ((c.to_bits() as u128) << 64)
            | ((d.to_bits() as u128) << 96)
    };
    let f64x2 = |a: f64, b: f64| (a.to_bits() as u128) | ((b.to_bits() as u128) << 64);
    jit_eq_interp(
        |a| {
            a.pblendvb(xmm1, xmm2).unwrap(); // byte blend by XMM0 byte MSBs
            a.blendvps(xmm3, xmm4).unwrap(); // dword blend
            a.blendvpd(xmm5, xmm6).unwrap(); // qword blend
            a.roundps(xmm7, xmm8, 0).unwrap(); // nearest-even
            a.roundps(xmm9, xmm8, 1).unwrap(); // floor
            a.roundpd(xmm10, xmm11, 2).unwrap(); // ceil
            a.roundss(xmm12, xmm13, 3).unwrap(); // truncate (scalar: keeps xmm12 lanes 1-3)
            a.roundsd(xmm14, xmm15, 1).unwrap(); // floor (scalar)
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0x80FF_0080_FF00_8000_00FF_8080_0000_00FF; // blend mask (mixed MSBs)
            c.xmm[1] = 0x1111_1111_1111_1111_1111_1111_1111_1111;
            c.xmm[2] = 0x2222_2222_2222_2222_2222_2222_2222_2222;
            c.xmm[3] = f32x4(1.0, 2.0, 3.0, 4.0);
            c.xmm[4] = f32x4(9.0, 9.0, 9.0, 9.0);
            c.xmm[5] = f64x2(1.0, 2.0);
            c.xmm[6] = f64x2(9.0, 9.0);
            c.xmm[8] = f32x4(2.5, -2.5, 3.5, -0.5);
            c.xmm[11] = f64x2(2.5, -2.5);
            c.xmm[12] = f32x4(7.7, 1.0, 2.0, 3.0); // scalar: lane0 rounded, rest kept
            c.xmm[13] = f32x4(2.9, 5.0, 5.0, 5.0);
            c.xmm[14] = f64x2(7.7, 8.8);
            c.xmm[15] = f64x2(-2.5, 5.0);
        },
        &[],
    );
}

/// SSE4.1 dword min/max (task-168.5.4): `pmin/pmax s/u d` reuse the existing packed
/// min/max ops at 32-bit lanes — signed and unsigned differ on the high-bit values.
#[test]
fn sse41_dword_minmax_match_interp() {
    const A: u128 = 0x8000_0000_0000_0001_FFFF_FFFF_7FFF_FFFF;
    const B: u128 = 0x0000_0001_8000_0000_0000_0002_7FFF_FFFE;
    jit_eq_interp(
        |a| {
            a.pminsd(xmm0, xmm1).unwrap();
            a.pmaxsd(xmm2, xmm3).unwrap();
            a.pminud(xmm4, xmm5).unwrap();
            a.pmaxud(xmm6, xmm7).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for r in [0, 2, 4, 6] {
                c.xmm[r] = A;
            }
            for r in [1, 3, 5, 7] {
                c.xmm[r] = B;
            }
        },
        &[],
    );
}

/// SSE4.1 `pmovzx`/`pmovsx` (register + memory source) and `pmulld` (task-168.5.4):
/// lane extension with distinct zero/sign results (the source has high-bit-set bytes)
/// and per-lane 32-bit multiply — JIT == interp.
#[test]
fn sse41_pmovx_pmulld_match_interp() {
    // Bytes with bit 7 set in some lanes so zero- and sign-extend differ.
    const SRC: u128 = 0x8000_7FFF_FE01_80FF_1234_5678_9ABC_DEF0;
    const M0: u128 = 0x0000_0002_FFFF_FFFF_0000_0003_8000_0000;
    const M1: u128 = 0x0000_0003_0000_0002_0000_0004_0000_0002;
    jit_eq_interp(
        |a| {
            a.pmovzxbw(xmm0, xmm1).unwrap(); // byte→word, zero-extend
            a.pmovsxbw(xmm2, xmm1).unwrap(); // byte→word, sign-extend
            a.pmovzxbd(xmm3, xmm1).unwrap(); // byte→dword, zero
            a.pmovsxwd(xmm4, xmm1).unwrap(); // word→dword, sign
            a.pmovsxdq(xmm5, xmm1).unwrap(); // dword→qword, sign
            a.pmovzxbq(xmm6, xmm1).unwrap(); // byte→qword, zero
                                             // Seed the scratch qword, then extend from memory.
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.pmovzxbw(xmm7, qword_ptr(SCRATCH)).unwrap(); // memory source (8 bytes)
            a.pmulld(xmm8, xmm9).unwrap(); // 4× 32-bit low-product
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = SRC;
            c.xmm[8] = M0;
            c.xmm[9] = M1;
            c.gpr[0] = 0x0102_8040_00FF_7F80; // scratch source bytes
        },
        &[],
    );
}

/// AVX-512 EVEX bitwise logic + `vpternlog` (task-168.5.2): `vpxorq/vpandq/vpord/
/// vpandnq` over 128- and 256-bit forms, and `vpternlog{d,q}` with two non-trivial
/// truth tables (0x96 = a^b^c, 0xE8 = bitwise majority). Results land in xmm/ymm the
/// snapshot compares directly — JIT == interp. (512-bit shares the same lane loop but
/// isn't observable until the snapshot grows ZMM fields, task-193.)
#[test]
fn avx512_evex_logic_and_ternlog_match_interp() {
    const P1: u128 = 0xF0F0_F0F0_0F0F_0F0F_AAAA_5555_1234_5678;
    const P2: u128 = 0x0FF0_1234_DEAD_BEEF_5A5A_A5A5_9999_0000;
    const H1: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    const H2: u128 = 0x8765_4321_0FED_CBA9_2468_ACE0_1357_9BDF;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // 128-bit EVEX logic, each into a distinct dst.
            a.vpxorq(xmm0, xmm1, xmm2).unwrap();
            a.vpandq(xmm3, xmm1, xmm2).unwrap();
            a.vpord(xmm4, xmm1, xmm2).unwrap();
            a.vpandnq(xmm5, xmm1, xmm2).unwrap();
            // 256-bit forms (both halves).
            a.vpxord(ymm6, ymm1, ymm2).unwrap();
            a.vpandnd(ymm7, ymm1, ymm2).unwrap();
            // vpternlog: dst is also the first source. xmm8 = xmm8 ^ xmm1 ^ xmm2 (0x96).
            a.vpternlogd(xmm8, xmm1, xmm2, 0x96).unwrap();
            // ymm9 = majority(ymm9, ymm1, ymm2) per bit (0xE8), both halves.
            a.vpternlogq(ymm9, ymm1, ymm2, 0xE8).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = P1;
            c.xmm[2] = P2;
            c.ymm_hi[1] = H1;
            c.ymm_hi[2] = H2;
            // ternlog first-source/destination seeds.
            c.xmm[8] = P2 ^ P1;
            c.xmm[9] = H1 ^ P2;
            c.ymm_hi[9] = H2 ^ P1;
        },
        &[],
    );
}

/// AVX `vptest` (task-168.4): the flags-only AND test Go's AVX2 memory routines
/// use. Covers all-zero (ZF=1), mixed, and 128- vs 256-bit forms — JIT == interp.
#[test]
fn avx_vptest_matches_interp() {
    const LO: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const HI: u128 = 0xFF00_FF00_1234_5678_9ABC_DEF0_0011_2233;
    jit_eq_interp(
        |a| {
            // Capture each ZF/CF into distinct registers (compared directly).
            a.vptest(ymm0, ymm0).unwrap(); // non-zero → ZF=0, CF=1
            a.setz(r8b).unwrap();
            a.setb(r9b).unwrap();
            a.vptest(ymm2, ymm3).unwrap(); // both zero → ZF=1, CF=1
            a.setz(r10b).unwrap();
            a.setb(r11b).unwrap();
            a.vptest(ymm0, ymm2).unwrap(); // b=0 → ZF=1, CF=1
            a.setz(r12b).unwrap();
            a.setb(r13b).unwrap();
            a.vptest(xmm0, xmm1).unwrap(); // 128-bit form
            a.setz(r14b).unwrap();
            a.setb(r15b).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = LO;
            c.ymm_hi[0] = HI;
            c.xmm[1] = HI;
            // ymm2, ymm3 left zero.
        },
        &[],
    );
}
