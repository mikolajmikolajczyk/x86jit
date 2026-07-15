//! JIT config-matrix acceptance (M4, testing.md §8.1): the Cranelift backend must
//! produce identical state to the interpreter on every input. The interpreter is
//! the oracle for the JIT (§8).

use iced_x86::code_asm::*;
use x86jit_core::jit_abi::run_compiled;
use x86jit_core::lift::{lift_block, CpuMode, LiftError};
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

/// task-223: `SAL` is the `/6` encoding alias of `SHL` (identical semantics). iced's
/// assembler emits the `/4` form for `.shl(...)`, so the `/6` alias is hand-encoded:
/// `C1 F0 03` = `sal eax, 3` and `D1 E3`→ use `C1 F3 01` = `sal ebx, 1`. Before the
/// fix the alias lifted to `UnknownInstruction`.
#[test]
fn sal_alias_matches_interp() {
    jit_eq_interp(
        |a| {
            a.mov(eax, 0x8001_0003u32 as i32).unwrap();
            a.db(&[0xC1, 0xF0, 0x03]).unwrap(); // sal eax, 3  (/6 alias)
            a.mov(ebx, 0x1i32).unwrap();
            a.db(&[0xC1, 0xF3, 0x1F]).unwrap(); // sal ebx, 31 (/6 alias)
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// task-223: `bsf`/`bsr` with a zero source set ZF=1 and leave the destination as its
/// prior value (the low `size` bits are preserved; a 32-bit form still zero-extends
/// the upper half through the normal register write). Exercise all three widths for
/// both mnemonics with a zero source so the "keep old dest" path is covered jit==interp.
#[test]
fn bsf_bsr_zero_source_preserves_dest() {
    jit_eq_interp(
        |a| {
            // 64-bit: full register preserved (source rbx = 0).
            a.mov(rax, 0xDEAD_BEEF_1234_5678u64).unwrap();
            a.xor(rbx, rbx).unwrap();
            a.bsf(rax, rbx).unwrap();
            // 32-bit: low 32 preserved, upper 32 zeroed by the write.
            a.mov(rcx, 0xCAFE_F00D_0BAD_C0DEu64).unwrap();
            a.xor(edx, edx).unwrap();
            a.bsr(ecx, edx).unwrap();
            // 16-bit: only the low 16 bits are the destination; upper bits preserved.
            a.mov(rsi, 0x1111_2222_3333_4444u64).unwrap();
            a.xor(edi, edi).unwrap();
            a.bsf(si, di).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// task-223: a 66h `push imm16` transfers 2 bytes (not the 8-byte long-mode default),
/// and 66h `leave` (`Leavew`) pops BP 16-bit with SP advancing by 2. Hand-encoded:
/// `66 68 34 12` = `push 0x1234` (imm16), `66 C9` = `leave` (op-size 16).
#[test]
fn op66_push_imm16_and_leave_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rsp, SCRATCH + 0x800).unwrap();
            a.db(&[0x66, 0x68, 0x34, 0x12]).unwrap(); // push imm16 0x1234
            a.movzx(ecx, word_ptr(rsp)).unwrap(); // observe the 2 pushed bytes
                                                  // 66h leave: rbp frame at scratch+0x900 holds a saved value.
            a.mov(rbp, SCRATCH + 0x900).unwrap();
            a.mov(rax, 0x4433_2211_DEAD_BEEFu64).unwrap();
            a.mov(qword_ptr(rbp), rax).unwrap();
            a.db(&[0x66, 0xC9]).unwrap(); // leave (op-size 16)
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

/// task-224: a variable-count shift/rotate with a runtime count of 0 must leave EVERY
/// flag untouched (§16). The differential suite is blind to this — interp and JIT could
/// be wrong the same way — so this test both asserts jit==interp AND pins the concrete
/// expected flag bits (= the pre-set values, i.e. the hardware truth: flags preserved).
///
/// It also exercises the dangerous elision-liveness direction: `cmp` sets all six flags,
/// then `<shift> reg, cl` (cl == 0) writes none of them, so `cmp`'s flags must NOT be
/// elided as dead — they are still live and observable at the block exit.
#[test]
fn count0_runtime_shift_preserves_flags() {
    // Pre-set a distinctive, self-consistent flag state via `cmp`, then run each
    // variable-count shift/rotate with cl == 0. Every flag must come out unchanged.
    // `cmp 0x80, 0x01` (8-bit): 0x80 - 0x01 = 0x7F. CF=0 (no borrow), ZF=0, SF=0
    // (bit7 of 0x7F is 0), OF=1 (signed -128 - 1 overflows i8), AF=1 (low-nibble
    // borrow: 0x0 - 0x1), PF=0 (0x7F has 7 set bits → odd parity). These are the
    // hardware truth we pin.
    const EXPECT: (bool, bool, bool, bool, bool, bool) =
        //  cf     pf     af    zf     sf     of
        (false, false, true, false, false, true);

    let one_shift = |shift: fn(&mut CodeAssembler)| {
        let build = |a: &mut CodeAssembler| {
            a.xor(ecx, ecx).unwrap(); // cl = 0 — a *runtime* count (a Temp in the IR)
            a.mov(edx, 0x1234_5678i32).unwrap();
            // Establish the known flag state immediately before the shift (nothing
            // between them touches flags), so "flags preserved across a count-0 shift"
            // is exactly what the exit state measures.
            a.mov(al, 0x80i32).unwrap();
            a.cmp(al, 0x01i32).unwrap();
            shift(a); // <shift> edx, cl  with cl == 0 — must touch NO flag
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
        let interp = run_with_backend(&input, Box::new(InterpreterBackend));
        let jit = run_with_backend(&input, Box::new(JitBackend::new()));
        // jit == interp (differential), but that alone can't prove hardware-correctness.
        if let Some(d) = compare(&interp, &jit, &[]) {
            panic!("JIT diverges from interpreter on count-0 shift:\n{d}");
        }
        // Pin the hardware truth: the count-0 shift preserved `cmp`'s exact flags.
        let f = interp.cpu.flags;
        assert_eq!(
            (f.cf, f.pf, f.af, f.zf, f.sf, f.of),
            EXPECT,
            "count-0 shift/rotate must leave all flags at their pre-set (cmp) values"
        );
    };

    one_shift(|a| a.shl(edx, cl).unwrap());
    one_shift(|a| a.shr(edx, cl).unwrap());
    one_shift(|a| a.sar(edx, cl).unwrap());
    one_shift(|a| a.rol(edx, cl).unwrap());
    one_shift(|a| a.ror(edx, cl).unwrap());
    one_shift(|a| a.rcl(edx, cl).unwrap());
    one_shift(|a| a.rcr(edx, cl).unwrap());
}

/// task-224 (elision): a flag *read* AFTER a possibly-no-op variable-count shift. Here
/// `cmp` is NOT the block's last flag writer, so plain dead-flag elision would treat the
/// intervening `shl edx, cl` as a definite CF/SF/ZF/... clobber and drop `cmp`'s flags as
/// dead. But with cl == 0 the shift writes nothing, so `setcc`/`cmovcc` must observe
/// `cmp`'s flags — the earlier producer is still live. This is the case the block-boundary
/// "everything live" rule does NOT cover, so it exercises the `elide_dead_flags` fix.
#[test]
fn count0_shift_keeps_prior_flags_live_for_later_read() {
    let build = |a: &mut CodeAssembler| {
        a.xor(ecx, ecx).unwrap(); // cl = 0 (runtime count)
        a.mov(edx, 0x1234_5678i32).unwrap();
        a.mov(al, 0x80i32).unwrap();
        a.cmp(al, 0x01i32).unwrap(); // CF=0, ZF=0, SF=0, OF=1 (see above)
        a.shl(edx, cl).unwrap(); // cl==0 → writes NO flags; must not elide `cmp`'s
                                 // Materialise the flags into GPRs so they are observable at the exit state.
        a.setb(bl).unwrap(); // CF  -> bl  (expect 0)
        a.sete(bh).unwrap(); // ZF  -> bh  (expect 0)
        a.seto(dil).unwrap(); // OF  -> dil (expect 1)
        a.sets(sil).unwrap(); // SF  -> sil (expect 0)
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
    let interp = run_with_backend(&input, Box::new(InterpreterBackend));
    let jit = run_with_backend(&input, Box::new(JitBackend::new()));
    if let Some(d) = compare(&interp, &jit, &[]) {
        panic!("JIT diverges from interpreter on count-0-shift flag read:\n{d}");
    }
    // Hardware truth: the reads see `cmp 0x80,0x01`'s flags, untouched by the count-0 shift.
    let g = interp.cpu.gpr;
    assert_eq!(g[3] & 0xff, 0, "CF (setb bl) must be 0"); // rbx low = bl
    assert_eq!((g[3] >> 8) & 0xff, 0, "ZF (sete bh) must be 0"); // rbx bits 8..16 = bh
    assert_eq!(g[7] & 0xff, 1, "OF (seto dil) must be 1"); // rdi low = dil
    assert_eq!(g[6] & 0xff, 0, "SF (sets sil) must be 0"); // rsi low = sil
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

/// task-164: non-temporal stores lower to plain stores — `movntdq`/`movntps`/`movntpd`
/// (16-byte vector) and `movnti` (GPR). Store to scratch, read back, jit==interp.
#[test]
fn movnt_stores_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movntdq(xmmword_ptr(rax), xmm1).unwrap();
            a.movntps(xmmword_ptr(rax + 16), xmm1).unwrap();
            a.movntpd(xmmword_ptr(rax + 32), xmm1).unwrap();
            a.movdqu(xmm3, xmmword_ptr(rax)).unwrap();
            a.movdqu(xmm4, xmmword_ptr(rax + 32)).unwrap();
            a.movnti(qword_ptr(rax + 48), rbx).unwrap();
            a.mov(rcx, qword_ptr(rax + 48)).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
            c.gpr[3] = 0xDEAD_BEEF_CAFE_F00D;
        },
        &[],
    );
}

/// task-189: 8-bit one-operand `mul r/m8` / `imul r/m8` (F6 /4,/5) — AL*src8 → AX
/// (AH:AL), CF/OF from a non-zero AH. Covers no-overflow, overflow, and signed cases.
/// The fuzzer (task-185) found this form unlifted; this pins the AH:AL + flag semantics.
#[test]
fn mul8_imul8_match_interp() {
    jit_eq_interp(
        |a| {
            // Dirty RAX upper bits so "only AX is written" is observable.
            a.mov(rax, 0x7777_7777_7777_00FFu64).unwrap();
            a.mov(bl, 0x12i32).unwrap();
            a.mul(bl).unwrap(); // 0xFF * 0x12 = 0x11EE -> AX, CF/OF set (AH != 0)
            a.mov(al, 5i32).unwrap();
            a.mov(cl, 6i32).unwrap();
            a.mul(cl).unwrap(); // 30 -> AX, AH == 0 -> CF/OF clear
            a.mov(al, -3i32).unwrap(); // 0xFD
            a.mov(dl, 4i32).unwrap();
            a.imul(dl).unwrap(); // signed -3 * 4 = -12 -> AX = 0xFFF4, CF/OF set
            a.mov(al, 2i32).unwrap();
            a.mov(bl, 3i32).unwrap();
            a.imul(bl).unwrap(); // 6 -> fits in AL, CF/OF clear
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// task-248: 8-bit one-operand `div r/m8` / `idiv r/m8` (F6 /6,/7) — AX / src8, quotient
/// → AL, remainder → AH (not the RDX:RAX split). Pins the AX-dividend read + AL:AH pack
/// and the signed path; `dil` reached via REX is the exact retail wall shape. Dirties
/// RAX[63:16] so "only AX read / AL:AH written" is observable across both tiers.
#[test]
fn div8_idiv8_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x7777_7777_7777_0000u64).unwrap();
            a.mov(ax, 1003i32).unwrap();
            a.mov(bl, 10i32).unwrap();
            a.div(bl).unwrap(); // 1003 / 10 = 100 rem 3 -> AL=100, AH=3
            a.mov(ax, (-100i32) & 0xFFFF).unwrap();
            a.mov(dil, (-7i32) & 0xFF).unwrap();
            a.idiv(dil).unwrap(); // signed via dil (REX): -100 / -7 = 14 rem -2
            a.mov(ax, (-100i32) & 0xFFFF).unwrap();
            a.mov(cl, 7i32).unwrap();
            a.idiv(cl).unwrap(); // mixed sign: -100 / 7 = -14 rem -2 -> AL=0xF2, AH=0xFE
            a.mov(ax, 0x7Fu16 as i32).unwrap();
            a.mov(cl, 1i32).unwrap();
            a.div(cl).unwrap(); // 127 / 1 = 127 rem 0 -> AL=0x7F, AH=0
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// Assemble `build` + `hlt`, run it on BOTH the interpreter and the JIT, and assert each
/// tier exits with #DE (vector 0). Shared by the 8-bit divide exception tests so the
/// exception path is oracle-checked on both tiers, not just one.
fn assert_div8_raises_de(build: impl Fn(&mut CodeAssembler)) {
    let mut asm = CodeAssembler::new(64).unwrap();
    build(&mut asm);
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    for jit in [false, true] {
        let mut vm = if jit {
            Vm::with_backend(VmConfig::flat(0x2000), Box::new(JitBackend::new()))
        } else {
            Vm::new(VmConfig::flat(0x2000))
        };
        vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        let tier = if jit { "jit" } else { "interp" };
        match cpu.run(&vm, Some(100)) {
            Exit::Exception { vector, .. } => assert_eq!(vector, 0, "#DE is vector 0 ({tier})"),
            other => panic!("expected #DE on {tier}, got {other:?}"),
        }
    }
}

/// task-248: 8-bit `div r/m8` by zero raises #DE (vector 0), like the wider forms.
#[test]
fn div8_by_zero_raises_de() {
    assert_div8_raises_de(|a| {
        a.mov(ax, 100i32).unwrap();
        a.mov(cl, 0i32).unwrap();
        a.div(cl).unwrap();
    });
}

/// task-248: 8-bit `div r/m8` quotient overflow raises #DE. AX=0x2000 / 1 = 0x2000, which
/// does not fit in AL (> 0xFF) — the architecture raises #DE before any register write.
#[test]
fn div8_overflow_raises_de() {
    assert_div8_raises_de(|a| {
        a.mov(ax, 0x2000i32).unwrap();
        a.mov(cl, 1i32).unwrap();
        a.div(cl).unwrap(); // 0x2000 / 1 = 0x2000 -> overflows AL
    });
}

/// task-248: 8-bit `idiv r/m8` quotient overflow raises #DE via the SIGNED bound (a
/// separate branch from the unsigned one above). AX=0x8000 = -32768, idiv by 1 = -32768,
/// far outside the signed AL range [-128, 127].
#[test]
fn idiv8_overflow_raises_de() {
    assert_div8_raises_de(|a| {
        a.mov(ax, 0x8000i32).unwrap();
        a.mov(cl, 1i32).unwrap();
        a.idiv(cl).unwrap(); // -32768 / 1 = -32768 -> out of [-128, 127]
    });
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
    // An unlifted instruction (`mpsadbw`, the SSE4.1 multi-block sum-of-abs-differences we
    // deliberately do not lift) must surface its actual opcode bytes in the lift error, not
    // 15 zeros — so compat triage isn't misdirected (#18). `ptest`, then `pcmpistri`, then
    // `dpps` used to sit here but are now lifted (task-168.4 / task-168.5.4 / task-195).
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mpsadbw(xmm0, xmm1, 0).unwrap();
    let code = asm.assemble(CODE).unwrap();

    let mut vm = Vm::with_backend(VmConfig::flat(0x2000), Box::new(InterpreterBackend));
    vm.map(CODE, 0x1000, Prot::RX, RegionKind::Ram).unwrap();
    vm.write_bytes(CODE, &code).unwrap();

    match lift_block(&vm.mem, CODE, CpuMode::Long64) {
        Err(LiftError::Unsupported { bytes, len, .. }) => {
            assert_ne!(bytes, [0u8; 15], "must not report 15 zero bytes");
            assert_eq!(
                &bytes[..len as usize],
                &code[..len as usize],
                "reported bytes must be the real mpsadbw opcode"
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

    let ir = lift_block(&vm.mem, CODE, CpuMode::Long64).expect("lift the block");
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
    match unsafe { run_compiled(entry, &mut cpu.cpu, &vm.mem, CpuMode::Long64) } {
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

/// x87 `fisttp` (SSE3, task-195): store ST(0) as an integer truncating toward zero (unlike
/// `fistp`, which rounds per the FPU control word), then pop. 13.7 → 13 (round would give
/// 14) and -2.9 → -2, at 16/32/64-bit widths. glibc number formatting (seq) uses it.
/// JIT == interp on the stored GPRs.
#[test]
fn x87_fisttp_truncates_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x402B_6666_6666_6666u64).unwrap(); // 13.7
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xC007_3333_3333_3333u64).unwrap(); // -2.9
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.fld(qword_ptr(SCRATCH)).unwrap(); // 13.7
            a.fisttp(qword_ptr(SCRATCH + 16)).unwrap(); // -> 13 (i64), pop
            a.fld(qword_ptr(SCRATCH + 8)).unwrap(); // -2.9
            a.fisttp(dword_ptr(SCRATCH + 24)).unwrap(); // -> -2 (i32), pop
            a.fld(qword_ptr(SCRATCH)).unwrap(); // 13.7
            a.fisttp(word_ptr(SCRATCH + 32)).unwrap(); // -> 13 (i16), pop
            a.mov(r8, qword_ptr(SCRATCH + 16)).unwrap();
            a.movsxd(r9, dword_ptr(SCRATCH + 24)).unwrap();
            a.movzx(r10d, word_ptr(SCRATCH + 32)).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// VEX-128 `vpmov{z,s}x*` (task-195): zero/sign-extend narrow → wide lanes with VEX's
/// upper-zeroing (bits 255:128 cleared). Register and memory sources; the mask-check on
/// the destination's high lane confirms the VEX zeroing. JIT == interp.
#[test]
fn vex_pmovx_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vpmovzxdq(xmm0, qword_ptr(rax)).unwrap(); // 2 dwords -> 2 qwords (mem)
            a.vpmovzxbw(xmm1, xmm3).unwrap(); // 8 bytes -> 8 words (reg)
            a.vpmovsxwd(xmm2, xmm3).unwrap(); // 4 words -> 4 dwords, signed
            a.vpmovzxbq(xmm4, qword_ptr(rax)).unwrap(); // 2 bytes -> 2 qwords (mem)
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[3] = 0x8001_7F02_00FF_8040_1020_4080_C0A0_9070;
            // Dirty the destinations' upper 128 bits so VEX zeroing is observable.
            c.ymm_hi[0] = u128::MAX;
            c.ymm_hi[1] = u128::MAX;
            c.ymm_hi[2] = u128::MAX;
            c.ymm_hi[4] = u128::MAX;
        },
        &[],
    );
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

/// Memory-source `src2` for the EVEX mask-producing compares (task-195): glibc folds the
/// second operand as a load (`vpcmpeqb k, zmm, [rsi]`). The B operand is staged into
/// SCRATCH, then each compare reads it from memory; masks move to GPRs so the opmask
/// results are compared JIT == interp across `vpcmpeqb`, `vpcmp[u]d`, and `vptestnmb`, at
/// 128- and 256-bit.
#[test]
fn avx512_vpcmp_vptest_mem_src_match_interp() {
    const A: u128 = 0x000E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const B: u128 = 0x800E_0D0C_0B0A_0908_0706_0504_0302_01FF;
    const HI_A: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    const HI_B: u128 = 0x1111_2222_3333_4444_5555_6666_7777_9999;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // Stage B (ymm1) into SCRATCH so the compares can fold it as a memory operand.
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm1).unwrap();
            // vpcmpeqb k1, xmm0, [scratch]  (128-bit, byte lanes)
            a.vpcmpeqb(k1, xmm0, xmmword_ptr(rax)).unwrap();
            a.kmovd(r8d, k1).unwrap();
            // vpcmpd k2, xmm0, [scratch], 6 (signed GT, dword lanes)
            a.vpcmpd(k2, xmm0, xmmword_ptr(rax), 6).unwrap();
            a.kmovd(r9d, k2).unwrap();
            // vpcmpud k3, xmm0, [scratch], 1 (unsigned LT, dword lanes)
            a.vpcmpud(k3, xmm0, xmmword_ptr(rax), 1).unwrap();
            a.kmovd(r10d, k3).unwrap();
            // vptestnmb k4, xmm0, [scratch]  ((a & b) == 0 per byte)
            a.vptestnmb(k4, xmm0, xmmword_ptr(rax)).unwrap();
            a.kmovd(r11d, k4).unwrap();
            // 256-bit form: vpcmpeqb k5, ymm0, [scratch] → 32 mask bits
            a.vpcmpeqb(k5, ymm0, ymmword_ptr(rax)).unwrap();
            a.kmovd(r12d, k5).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = HI_A;
            c.xmm[1] = B;
            c.ymm_hi[1] = HI_B;
        },
        &[],
    );
}

/// Memory-source `src2`/`src3` for the unmasked EVEX data ops (task-195), at 512-bit:
/// `vpxorq`/`vpternlogd` (logic), `vpaddq` (packed arith — the 512-bit form was entirely
/// unlifted), and `vpbroadcastw zmm, [mem]` (element broadcast). glibc folds these operands
/// as loads. Operands are staged in SCRATCH; results left in ZMM are compared JIT == interp.
#[test]
fn avx512_mem_src_data_ops_match_interp() {
    const A: u128 = 0xF0F0_F0F0_0F0F_0F0F_AAAA_5555_1234_5678;
    const A_HI: u128 = 0x0FF0_1234_DEAD_BEEF_5A5A_A5A5_9999_0000;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            // Stage zmm0 = {A, A_HI, A, A_HI} (512 bits) into SCRATCH, then fold as memory.
            a.vmovdqu64(zmmword_ptr(rax), zmm0).unwrap();
            a.vpxorq(zmm1, zmm0, zmmword_ptr(rax)).unwrap(); // a ^ a == 0
            a.vpternlogd(zmm2, zmm0, zmmword_ptr(rax), 0x96).unwrap(); // xor3 truth table
            a.vpaddq(zmm3, zmm0, zmmword_ptr(rax)).unwrap(); // 512-bit packed add
            a.vpbroadcastw(zmm4, word_ptr(rax)).unwrap(); // broadcast low word across 512
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = A_HI;
            c.zmm_hi[0] = [A, A_HI];
            // zmm2 is also a source of vpternlogd (dst is src1); give it a known value.
            c.xmm[2] = A_HI;
            c.ymm_hi[2] = A;
            c.zmm_hi[2] = [A_HI, A];
        },
        &[],
    );
}

/// AVX-512 write-masked **memory** moves (task-168.5.5): `vmovdqu8 v{k}{z}, [mem]` (load,
/// zeroing and merge) and `[mem]{k}, v` (store). Element-wise so masked-off lanes are
/// zeroed/kept and never touch memory. Staged through SCRATCH: store A, masked-load it two
/// ways, masked-store A into a second slot, read that slot back — all vector results are
/// compared JIT == interp.
#[test]
fn avx512_masked_mem_move_match_interp() {
    const A: u128 = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
    const A_HI: u128 = 0x1F1E_1D1C_1B1A_1918_1716_1514_1312_1110;
    const MERGE: u128 = 0xEEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm0).unwrap(); // stage A (ymm0) at [scratch]
            a.mov(ecx, 0x000F_A5A5u32).unwrap();
            a.kmovd(k1, ecx).unwrap(); // 32-bit byte mask over the 256-bit operand
                                       // masked load, zeroing: inactive byte lanes → 0.
            a.vmovdqu8(ymm1.k1().z(), ymmword_ptr(rax)).unwrap();
            // masked load, merge: inactive byte lanes keep ymm2's prior bytes.
            a.vmovdqu8(ymm2.k1(), ymmword_ptr(rax)).unwrap();
            // masked store into [scratch+32]; inactive byte lanes stay 0 (SCRATCH is zeroed).
            a.vmovdqu8(ymmword_ptr(rax + 32).k1(), ymm0).unwrap();
            a.vmovdqu(ymm3, ymmword_ptr(rax + 32)).unwrap(); // read the store result back
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = A_HI;
            c.xmm[2] = MERGE; // merge base (low 128)
            c.ymm_hi[2] = MERGE; // merge base (high 128)
        },
        &[],
    );
}

/// AVX-512 ops the real v4 coreutils corpus hits (task-195): per-lane population count
/// `vpopcnt{d,q}`, opmask interleave `kunpckbw`, two-table permute `vpermt2d`, and the
/// 256-bit lane extract `vextracti32x8`. Full 512-bit inputs come from the init snapshot;
/// results (ZMM + a GPR-materialized opmask) are compared JIT == interp.
#[test]
fn avx512_permute_popcnt_kunpck_match_interp() {
    const L0: u128 = 0xF0F0_FFFF_0001_8000_DEAD_BEEF_1234_5678;
    const L1: u128 = 0x0000_0000_FFFF_FFFF_8080_8080_0101_0101;
    const L2: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    const L3: u128 = 0x0102_0408_1020_4080_FEFE_FEFE_AAAA_5555;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // vpopcntq/d over the full 512-bit zmm0 → zmm5 / zmm6.
            a.vpopcntq(zmm5, zmm0).unwrap();
            a.vpopcntd(zmm6, zmm0).unwrap();
            // kunpckbw k3, k1, k2 → (k1_low8 << 8) | k2_low8; materialize into a GPR.
            a.mov(eax, 0x00A5u32).unwrap();
            a.kmovd(k1, eax).unwrap();
            a.mov(eax, 0x005Au32).unwrap();
            a.kmovd(k2, eax).unwrap();
            a.kunpckbw(k3, k1, k2).unwrap();
            a.kmovd(r8d, k3).unwrap();
            // vpermt2d zmm2{}, zmm3(index), zmm1(table1); zmm2 is table0 + result.
            a.vpermt2d(zmm2, zmm3, zmm1).unwrap();
            // vextracti32x8 ymm7, zmm0, 1 → the high 256-bit lane of zmm0.
            a.vextracti32x8(ymm7, zmm0, 1).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = L0;
            c.ymm_hi[0] = L1;
            c.zmm_hi[0] = [L2, L3];
            c.xmm[1] = L3;
            c.ymm_hi[1] = L2;
            c.zmm_hi[1] = [L1, L0];
            // zmm3 = per-dword indices into the 32-lane {zmm2, zmm1} table.
            c.xmm[3] = 0x0000_0011_0000_0002_0000_0013_0000_0004;
            c.ymm_hi[3] = 0x0000_0005_0000_0016_0000_0007_0000_0018;
            c.zmm_hi[3] = [
                0x0000_0009_0000_001A_0000_000B_0000_001C,
                0x0000_000D_0000_001E_0000_000F_0000_0000,
            ];
            c.xmm[2] = L1;
            c.ymm_hi[2] = L0;
            c.zmm_hi[2] = [L3, L2];
        },
        &[],
    );
}

/// VEX/EVEX scalar float arithmetic + int conversions the coreutils corpus hits (task-195):
/// 3-operand `vmulsd`/`vaddsd`, `vmovsd` merge, the unsigned conversions `vcvtusi2sd` /
/// `vcvttsd2usi` (which glibc's number formatting uses), and `vcomisd`'s flags. The unsigned
/// input exceeds i64::MAX so the signed vs unsigned paths differ. Compared JIT == interp.
#[test]
fn avx512_vex_float_and_unsigned_cvt_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // rdx = 0xC000_0000_0000_0000 (> i64::MAX): unsigned→double must not go negative.
            a.mov(rdx, 0xC000_0000_0000_0000u64).unwrap();
            a.vcvtusi2sd(xmm0, xmm0, rdx).unwrap(); // xmm0 = (double)rdx (unsigned)
            a.vmulsd(xmm1, xmm0, xmm2).unwrap(); // xmm1 = xmm0 * xmm2
            a.vaddsd(xmm3, xmm1, xmm0).unwrap(); // xmm3 = xmm1 + xmm0
            a.vcvttsd2usi(r8, xmm0).unwrap(); // r8 = (u64)xmm0 truncated
            a.vcomisd(xmm0, xmm2).unwrap(); // set ZF/PF/CF from the compare
            a.setb(r9b).unwrap(); // capture CF into a GPR
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0; // built in-snippet from rdx
            c.xmm[2] = 2.5f64.to_bits() as u128; // multiplier
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

/// MMX↔XMM bridge `movq2dq` / `movdq2q` (task-208): XMM→MMX→XMM round-trip through the
/// aliased x87 register. JIT must match interp bit-for-bit (the Unicorn differential
/// validates the aliasing against a HW-ish oracle; the native oracle can't capture x87).
#[test]
fn mmx_bridge_match_interp() {
    const A: u128 = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_FF00;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.movdq2q(mm0, xmm0).unwrap(); // xmm0.lo -> mm0 (fpr[0])
            a.movdq2q(mm5, xmm1).unwrap(); // xmm1.lo -> mm5 (fpr[5])
            a.movq2dq(xmm2, mm0).unwrap(); // mm0 -> xmm2 (upper zeroed)
            a.movq2dq(xmm3, mm5).unwrap(); // mm5 -> xmm3
            a.emms().unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.xmm[1] = !A;
        },
        &[],
    );
}

/// Masked EVEX unary lane ops `vplzcnt{d,q}` / `vprol{d,q}` / `vpconflict{d,q}` (task-209),
/// unmasked + masked-merge + zeroing at 128/256-bit. JIT must match interp bit-for-bit
/// (the native oracle validates the lane math + opmask semantics against real hardware).
#[test]
fn vp_unary_lane_variants_match_interp() {
    const A: u128 = 0x0000_0002_0000_0002_0000_0100_0000_0002; // dwords w/ repeats for conflict
    const D: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10; // merge-dst seed
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010i32).unwrap();
            a.kmovw(k1, eax).unwrap(); // partial mask
            a.vplzcntd(xmm0, xmm1).unwrap();
            a.vplzcntq(xmm2, xmm1).unwrap();
            a.vprold(xmm3, xmm1, 7).unwrap();
            a.vprolq(xmm4, xmm1, 13).unwrap();
            a.vpconflictd(xmm5, xmm1).unwrap();
            a.vplzcntd(xmm6.k1(), xmm1).unwrap(); // merge
            a.vprold(xmm7.k1().z(), xmm1, 3).unwrap(); // zeroing
            a.vpconflictd(xmm8.k1().z(), xmm1).unwrap(); // zeroing
            a.vplzcntd(ymm9, ymm1).unwrap(); // 256-bit
            a.vprold(ymm10.k1(), ymm1, 5).unwrap(); // 256-bit masked merge
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.ymm_hi[1] = A ^ 0x11;
            for r in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
                c.xmm[r] = D;
                c.ymm_hi[r] = D;
            }
        },
        &[],
    );
}

/// Masked EVEX blend `vpblendm{d,q}` (task-209), merge + zeroing at 128/256-bit. JIT must
/// match interp bit-for-bit (native oracle validates the blend-control against hardware).
#[test]
fn vp_blendm_variants_match_interp() {
    const A: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    const B: u128 = 0x9999_AAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000;
    const D: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010i32).unwrap();
            a.kmovw(k1, eax).unwrap();
            a.vpblendmd(xmm0.k1(), xmm1, xmm2).unwrap(); // merge
            a.vpblendmq(xmm3.k1().z(), xmm1, xmm2).unwrap(); // zeroing
            a.vpblendmd(xmm4.k1().z(), xmm1, xmm2).unwrap(); // zeroing
            a.vpblendmd(ymm5.k1(), ymm1, ymm2).unwrap(); // 256-bit merge
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.xmm[2] = B;
            c.ymm_hi[1] = B;
            c.ymm_hi[2] = A;
            for r in [0, 3, 4, 5] {
                c.xmm[r] = D;
                c.ymm_hi[r] = D;
            }
        },
        &[],
    );
}

/// Masked EVEX 128-bit-lane shuffle `vshuff32x4` / `vshuff64x2` (task-209) at 256-bit,
/// unmasked + masked merge + zeroing. JIT must match interp bit-for-bit (native oracle
/// validates the lane selection against hardware).
#[test]
fn vshuf_lane_variants_match_interp() {
    const A: u128 = 0x1111_1111_2222_2222_3333_3333_4444_4444;
    const AH: u128 = 0x5555_5555_6666_6666_7777_7777_8888_8888;
    const B: u128 = 0x9999_9999_AAAA_AAAA_BBBB_BBBB_CCCC_CCCC;
    const BH: u128 = 0xDDDD_DDDD_EEEE_EEEE_FFFF_FFFF_0000_0000;
    const D: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010i32).unwrap();
            a.kmovw(k1, eax).unwrap();
            a.vshuff32x4(ymm0, ymm1, ymm2, 0b11).unwrap(); // low from a lane1, high from b lane1
            a.vshuff64x2(ymm3, ymm1, ymm2, 0b01).unwrap();
            a.vshuff32x4(ymm4.k1(), ymm1, ymm2, 0b10).unwrap(); // merge
            a.vshuff32x4(ymm5.k1().z(), ymm1, ymm2, 0b01).unwrap(); // zeroing
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.ymm_hi[1] = AH;
            c.xmm[2] = B;
            c.ymm_hi[2] = BH;
            for r in [0, 3, 4, 5] {
                c.xmm[r] = D;
                c.ymm_hi[r] = D;
            }
        },
        &[],
    );
}

/// Masked EVEX `vpmultishiftqb` (task-209) at 128-bit, unmasked + masked merge + zeroing.
/// JIT must match interp bit-for-bit (native oracle validates the byte gather against
/// hardware).
#[test]
fn vp_multishift_variants_match_interp() {
    const CTRL: u128 = 0x0038_0030_0028_0020_0018_0010_0008_0000; // per-byte shifts
    const DATA: u128 = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
    const D: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010_1100_0011_0101i32).unwrap();
            a.kmovw(k1, eax).unwrap();
            a.vpmultishiftqb(xmm0, xmm1, xmm2).unwrap();
            a.vpmultishiftqb(xmm3.k1(), xmm1, xmm2).unwrap(); // merge
            a.vpmultishiftqb(xmm4.k1().z(), xmm1, xmm2).unwrap(); // zeroing
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = CTRL;
            c.xmm[2] = DATA;
            for r in [0, 3, 4] {
                c.xmm[r] = D;
            }
        },
        &[],
    );
}

/// FMA3 `vf[n]m{add,sub}{132,213,231}{ss,sd,ps,pd}` (task-201): fused multiply-add across
/// all operand orders, sign variants, scalar/packed types, and a memory operand. JIT ==
/// interp (the native oracle validates the fused rounding against real hardware).
#[test]
fn fma_all_variants_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // scalar double: three orders
            a.vfmadd132sd(xmm3, xmm1, xmm2).unwrap();
            a.vfmadd213sd(xmm4, xmm1, xmm2).unwrap();
            a.vfmadd231sd(xmm5, xmm1, xmm2).unwrap();
            // sign variants (scalar double)
            a.vfmsub132sd(xmm6, xmm1, xmm2).unwrap();
            a.vfnmadd213sd(xmm7, xmm1, xmm2).unwrap();
            a.vfnmsub231sd(xmm8, xmm1, xmm2).unwrap();
            // packed pd + packed ps + scalar ss
            a.vfmadd213pd(xmm9, xmm1, xmm2).unwrap();
            a.vfmadd213ps(xmm10, xmm1, xmm2).unwrap();
            a.vfmadd132ss(xmm11, xmm1, xmm2).unwrap();
            // memory operand (231, y from mem)
            a.movupd(xmmword_ptr(SCRATCH), xmm2).unwrap();
            a.vfmadd231pd(xmm12, xmm1, xmmword_ptr(SCRATCH)).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            // seed all involved regs with finite doubles/singles
            c.xmm[1] = (1.5f64).to_bits() as u128 | (((2.5f64).to_bits() as u128) << 64);
            c.xmm[2] = (-0.75f64).to_bits() as u128 | (((3.25f64).to_bits() as u128) << 64);
            for r in 3..=12 {
                c.xmm[r] = (0.5f64).to_bits() as u128 | (((-1.5f64).to_bits() as u128) << 64);
            }
        },
        &[],
    );
}

/// EVEX lane broadcast `vbroadcast{i,f}{32x4,64x2,32x8,64x4}` (task-214), memory chunk,
/// unmasked + masked merge + zeroing. JIT must match interp bit-for-bit (native oracle
/// validates the replication + mask vs hardware).
#[test]
fn broadcast_lane_variants_match_interp() {
    const D: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // Stage a 32-byte chunk at SCRATCH.
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0x99AA_BBCC_DDEE_FF00u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.mov(rax, 0x0011_2233_4455_6677u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 16), rax).unwrap();
            a.mov(qword_ptr(SCRATCH + 24), rax).unwrap();
            a.mov(eax, 0b1010i32).unwrap();
            a.kmovw(k1, eax).unwrap();
            // 128-bit chunk → ymm/zmm.
            a.vbroadcasti64x2(ymm0, xmmword_ptr(SCRATCH)).unwrap();
            a.vbroadcasti32x4(zmm2, xmmword_ptr(SCRATCH)).unwrap();
            // 256-bit chunk → zmm.
            a.vbroadcasti64x4(zmm3, ymmword_ptr(SCRATCH)).unwrap();
            // Masked merge + zeroing (dst pre-seeded).
            a.vbroadcasti32x4(zmm4.k1(), xmmword_ptr(SCRATCH)).unwrap();
            a.vbroadcasti64x2(zmm5.k1().z(), xmmword_ptr(SCRATCH))
                .unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for r in [0, 2, 3, 4, 5] {
                c.xmm[r] = D;
                c.ymm_hi[r] = D;
            }
        },
        &[],
    );
}

/// Masked EVEX packed FMA `vfmadd/vfmsub/vfnmadd{132,213,231}{ps,pd}` with a write-mask
/// (merge + zeroing) at 128/256-bit + a masked memory operand (task-201 AC#3). JIT must
/// match interp bit-for-bit (native oracle validates the fused rounding vs hardware).
#[test]
fn fma_masked_variants_match_interp() {
    const A: u128 = 0x4000_0000_0000_0000_3FF8_0000_0000_0000; // [1.5, 2.0]
    const B: u128 = 0xBFE0_0000_0000_0000_400A_0000_0000_0000; // [3.25, -0.5]
    const D: u128 = 0x3FE0_0000_0000_0000_C002_0000_0000_0000; // merge base
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b10i32).unwrap();
            a.kmovw(k1, eax).unwrap(); // lane 0 masked-off, lane 1 active
            a.vfmadd132pd(xmm0.k1(), xmm1, xmm2).unwrap(); // 128 merge
            a.vfmadd213pd(xmm3.k1().z(), xmm1, xmm2).unwrap(); // 128 zeroing
            a.vfmsub231ps(ymm4.k1(), ymm1, ymm2).unwrap(); // 256 ps merge
            a.vfnmadd213ps(ymm5.k1().z(), ymm1, ymm2).unwrap(); // 256 ps zeroing
                                                                // masked memory operand (231, y from mem).
            a.movupd(xmmword_ptr(SCRATCH), xmm2).unwrap();
            a.vfmadd231pd(xmm6.k1(), xmm1, xmmword_ptr(SCRATCH))
                .unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.xmm[2] = B;
            c.ymm_hi[1] = B;
            c.ymm_hi[2] = A;
            for r in [0, 3, 4, 5, 6] {
                c.xmm[r] = D;
                c.ymm_hi[r] = D;
            }
        },
        &[],
    );
}

/// Dword packed min/max `vpmin/max{u,s}d` (VEX.128 + EVEX-512, task-195): perl/python3
/// hit vpminud. Register src across widths. JIT == interp.
#[test]
fn avx512_dword_minmax_match_interp() {
    const A: u128 = 0x8000_0000_7FFF_FFFF_0000_0002_FFFF_FFFE;
    const B: u128 = 0x0000_0001_8000_0000_0000_0003_0000_0005;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpminud(xmm0, xmm1, xmm2).unwrap(); // VEX.128 unsigned min
            a.vpmaxud(xmm3, xmm1, xmm2).unwrap();
            a.vpminsd(xmm4, xmm1, xmm2).unwrap(); // signed
            a.vpmaxsd(xmm5, xmm1, xmm2).unwrap();
            a.vpminud(zmm6, zmm1, zmm2).unwrap(); // EVEX-512
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.xmm[2] = B;
            c.ymm_hi[1] = B;
            c.ymm_hi[2] = A;
            c.zmm_hi[1] = [A, B];
            c.zmm_hi[2] = [B, A];
        },
        &[],
    );
}

/// Cross-lane permutes (task-195): index-mode `vpermi2d`, single-source vector-index
/// `vpermq`/`vpermd` (EVEX-512), and memory-source `vpermt2d`. JIT == interp.
#[test]
fn avx512_permute_family_match_interp() {
    const L0: u128 = 0x0000_0003_0000_0002_0000_0001_0000_0000;
    const L1: u128 = 0x0000_0007_0000_0006_0000_0005_0000_0004;
    const L2: u128 = 0x0000_000B_0000_000A_0000_0009_0000_0008;
    const L3: u128 = 0x0000_000F_0000_000E_0000_000D_0000_000C;
    // qword index (low 3 bits used): reverse-ish selection.
    const QI: u128 = 0x0000_0000_0000_0007_0000_0000_0000_0001;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpermq(zmm4, zmm3, zmm0).unwrap(); // single-source qword permute (idx=zmm3)
            a.vpermd(zmm5, zmm3, zmm0).unwrap(); // single-source dword permute
            a.vpermi2d(zmm6, zmm1, zmm2).unwrap(); // index-mode (idx = old zmm6)
            a.vpermt2d(zmm7, zmm1, zmmword_ptr(SCRATCH)).unwrap(); // mem-source table1
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = L0;
            c.ymm_hi[0] = L1;
            c.zmm_hi[0] = [L2, L3];
            // index registers
            c.xmm[3] = QI;
            c.ymm_hi[3] = QI;
            c.zmm_hi[3] = [QI, QI];
            for r in [1, 2, 6, 7] {
                c.xmm[r] = L3;
                c.ymm_hi[r] = L2;
                c.zmm_hi[r] = [L1, L0];
            }
        },
        &[],
    );
}

/// VEX-128 grab-bag the python3 SIMD paths hit (task-195): 3-operand `vinserti128` from
/// memory, `vpblendw`, `vpackusdw`/`vpacksswb` saturating packs, and scalar `vsqrtsd`.
/// The mem operand is staged in SCRATCH. JIT == interp.
#[test]
fn avx_vinsert_blend_pack_sqrt_match_interp() {
    const A: u128 = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_FF00;
    const B: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.movdqu(xmmword_ptr(SCRATCH), xmm2).unwrap(); // stage a 128-bit lane
            a.vinserti128(ymm0, ymm1, xmmword_ptr(SCRATCH), 1).unwrap();
            a.vpblendw(xmm3, xmm1, xmm2, 0x5A).unwrap();
            a.vpackusdw(xmm4, xmm1, xmm2).unwrap(); // dword → word unsigned-sat
            a.vpacksswb(xmm5, xmm1, xmm2).unwrap(); // word → byte signed-sat
            a.vsqrtsd(xmm6, xmm1, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.xmm[2] = B;
            c.ymm_hi[1] = B;
            // a valid positive double in xmm2's low qword for the sqrt
            c.xmm[2] = (2.25f64).to_bits() as u128;
            for r in [0, 3, 4, 5, 6] {
                c.ymm_hi[r] = u128::MAX; // prove VEX upper-zeroing
            }
        },
        &[],
    );
}

/// `vpshufd` on ymm/zmm (task-195, python3): per-128-bit-lane dword shuffle, unmasked +
/// masked. JIT == interp.
#[test]
fn avx512_vpshufd_wide_match_interp() {
    const L0: u128 = 0x000102030405060708090A0B0C0D0E0F;
    const L1: u128 = 0x101112131415161718191A1B1C1D1E1F;
    const L2: u128 = 0x202122232425262728292A2B2C2D2E2F;
    const L3: u128 = 0x303132333435363738393A3B3C3D3E3F;
    const D: u128 = 0xA0A1A2A3A4A5A6A7A8A9AAABACADAEAF;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010_0101i32).unwrap();
            a.kmovw(k1, eax).unwrap();
            a.vpshufd(ymm4, ymm0, 0x1B).unwrap(); // reverse dwords per lane
            a.vpshufd(zmm5, zmm0, 0x4E).unwrap(); // swap dword pairs
            a.vpshufd(zmm6.k1().z(), zmm0, 0x1B).unwrap(); // zero-masked
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = L0;
            c.ymm_hi[0] = L1;
            c.zmm_hi[0] = [L2, L3];
            c.xmm[6] = D;
            c.ymm_hi[6] = D;
            c.zmm_hi[6] = [D, D];
        },
        &[],
    );
}

/// EVEX-512 widening move `vpmovsxdq zmm←ymm` + `vpmovzxbw ymm←xmm`, and the narrowing
/// store `vpmovqd [mem]←xmm` (task-195). The store result is reloaded into a register so
/// it is observable in the snapshot. JIT == interp.
#[test]
fn avx512_pmov_wide_and_narrow_mem_match_interp() {
    const L0: u128 = 0x8000_0001_7FFF_FFFF_0000_0002_FFFF_FFFE; // dwords incl. sign bits
    const L1: u128 = 0x0000_0003_FFFF_FFFD_1234_5678_9ABC_DEF0;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpmovsxdq(zmm4, ymm0).unwrap(); // 8 signed dwords → 8 qwords (zmm)
            a.vpmovzxbw(ymm5, xmm0).unwrap(); // 16 bytes → 16 zero-extended words (ymm)
            a.vpmovqd(xmmword_ptr(SCRATCH), xmm0).unwrap(); // 2 qwords → 2 dwords, to memory
            a.movq(xmm6, qword_ptr(SCRATCH)).unwrap(); // reload the 8-byte store result
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = L0;
            c.ymm_hi[0] = L1;
        },
        &[],
    );
}

/// AVX-512DQ `vpmullq` (64-bit packed multiply-low) + packed absolute value
/// `vpabs{b,d,q}` (task-195): openssl/curl hit vpmullq, vim hits vpabsb. JIT == interp.
#[test]
fn avx512_vpmullq_vpabs_match_interp() {
    const A: u128 = 0x8000_0000_0000_0003_FFFF_FFFF_FFFF_FFFE;
    const B: u128 = 0x0000_0000_0000_0007_0000_0000_0000_0005;
    const N: u128 = 0x80FF_7F01_8000_0000_FE02_04F8_1234_ABCD; // mixed signs for abs
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpmullq(zmm3, zmm1, zmm2).unwrap(); // 64-bit multiply-low, full zmm
            a.vpabsb(zmm4, zmm0).unwrap(); // |byte| per lane
            a.vpabsd(ymm5, ymm0).unwrap(); // |dword| per lane, ymm
            a.vpabsq(xmm6, xmm0).unwrap(); // |qword| per lane
            a.hlt().unwrap();
        },
        |c| {
            for r in [1, 2] {
                c.xmm[r] = if r == 1 { A } else { B };
                c.ymm_hi[r] = if r == 1 { B } else { A };
                c.zmm_hi[r] = if r == 1 { [A, B] } else { [B, A] };
            }
            c.xmm[0] = N;
            c.ymm_hi[0] = N;
            c.zmm_hi[0] = [N, N];
        },
        &[],
    );
}

/// EVEX-512 `vpshufb zmm` per-128-bit-lane byte shuffle (task-195, cal), unmasked +
/// merge/zero write-masking. Each result byte comes from its lane's control (MSB set →
/// zero). JIT == interp across all four 128-bit lanes.
#[test]
fn avx512_vpshufb_wide_match_interp() {
    const L0: u128 = 0x000102030405060708090A0B0C0D0E0F;
    const L1: u128 = 0x101112131415161718191A1B1C1D1E1F;
    const L2: u128 = 0x202122232425262728292A2B2C2D2E2F;
    const L3: u128 = 0x303132333435363738393A3B3C3D3E3F;
    // Control: mix of in-lane indices and MSB-set (→ zero) selectors.
    const C0: u128 = 0x8000010280030405_0680070880090A0B;
    const D: u128 = 0xA0A1A2A3A4A5A6A7A8A9AAABACADAEAF;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, 0x0F0F_0F0F_0F0F_0F0Fu64 as i64).unwrap();
            a.kmovq(k1, rax).unwrap();
            a.vpshufb(zmm4, zmm0, zmm1).unwrap(); // unmasked, full zmm
            a.vpshufb(zmm5.k1(), zmm0, zmm1).unwrap(); // merge-masked
            a.vpshufb(zmm6.k1().z(), zmm0, zmm1).unwrap(); // zero-masked
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = L0;
            c.ymm_hi[0] = L1;
            c.zmm_hi[0] = [L2, L3];
            // control replicated across all lanes so every 128-bit lane shuffles
            c.xmm[1] = C0;
            c.ymm_hi[1] = C0;
            c.zmm_hi[1] = [C0, C0];
            for r in [5, 6] {
                c.xmm[r] = D;
                c.ymm_hi[r] = D;
                c.zmm_hi[r] = [D, D];
            }
        },
        &[],
    );
}

/// Opmask shift `kshift{l,r}{b,w,d,q}` (task-195, vim): shift by imm8 within the width,
/// including a shift ≥ width that clears the mask. Materialized into GPRs. JIT == interp.
#[test]
fn avx512_kshift_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0xF0F0u32 as i32).unwrap();
            a.kmovd(k1, eax).unwrap();
            a.kshiftld(k2, k1, 3).unwrap(); // << 3 within 32 bits
            a.kshiftrd(k3, k1, 5).unwrap(); // >> 5
            a.kshiftlw(k4, k1, 20).unwrap(); // ≥ 16 → cleared
            a.kshiftrq(k5, k1, 1).unwrap();
            a.kmovd(r8d, k2).unwrap();
            a.kmovd(r9d, k3).unwrap();
            a.kmovd(r10d, k4).unwrap();
            a.hlt().unwrap();
        },
        |_c| {},
        &[],
    );
}

/// Opmask bitwise logic family `k{or,and,andn,xor,xnor}{b,d}` + `knot` (task-195): glibc's
/// AVX-512 string routines combine per-chunk compare masks with these. Each result is left
/// in an opmask (compared directly via the kmask snapshot) and a couple materialized into
/// GPRs. JIT == interp.
#[test]
fn avx512_opmask_logic_family_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0xF0F0u32 as i32).unwrap();
            a.kmovd(k1, eax).unwrap();
            a.mov(eax, 0x3C5Au32 as i32).unwrap();
            a.kmovd(k2, eax).unwrap();
            a.kord(k3, k1, k2).unwrap(); // 32-bit OR
            a.korb(k4, k1, k2).unwrap(); // 8-bit OR (high bits cleared)
            a.kandd(k5, k1, k2).unwrap();
            a.kandnd(k6, k1, k2).unwrap(); // ~k1 & k2
            a.kxord(k7, k1, k2).unwrap();
            a.kxnord(k1, k1, k2).unwrap(); // overwrites k1 last
            a.knotd(k2, k2).unwrap();
            a.kmovd(r8d, k3).unwrap();
            a.kmovd(r9d, k6).unwrap();
            a.hlt().unwrap();
        },
        |_c| {},
        &[],
    );
}

/// EVEX narrowing (truncating) move `vpmov{dw,qd,wb}` (task-195), unmasked + merge/zero
/// write-masking. Truncates each source lane to its low bytes, packs into the low lanes,
/// zeroes above; masked-off result lanes keep the old dst (merge) or clear (zeroing).
#[test]
fn avx512_narrowing_move_match_interp() {
    const L0: u128 = 0xF0F0_FFFF_0001_8000_DEAD_BEEF_1234_5678;
    const L1: u128 = 0x0000_0000_FFFF_FFFF_8080_8080_0101_0101;
    const L2: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    const L3: u128 = 0x0102_0408_1020_4080_FEFE_FEFE_AAAA_5555;
    const D: u128 = 0x0A0B_0C0D_0E0F_1011_1213_1415_1617_1819;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010_0110i32).unwrap();
            a.kmovw(k1, eax).unwrap();
            a.vpmovdw(ymm4, zmm0).unwrap(); // 16 dwords → 16 words (256-bit result)
            a.vpmovqd(ymm5, zmm0).unwrap(); // 8 qwords → 8 dwords (256-bit result)
            a.vpmovwb(xmm6, ymm0).unwrap(); // 16 words → 16 bytes (128-bit result)
            a.vpmovdw(ymm7.k1(), zmm0).unwrap(); // merge-masked
            a.vpmovqd(ymm8.k1().z(), zmm0).unwrap(); // zero-masked
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = L0;
            c.ymm_hi[0] = L1;
            c.zmm_hi[0] = [L2, L3];
            for r in [7, 8] {
                c.xmm[r] = D;
                c.ymm_hi[r] = D;
            }
        },
        &[],
    );
}

/// EVEX masked packed arithmetic `vpaddd`/`vpsubd`/`vpminud` under a write-mask
/// (task-168.5.5): compute per-lane then merge/zero-mask. Covers partial, all-ones and
/// all-zero masks across 128/256/512-bit widths. JIT == interp.
#[test]
fn avx512_masked_packed_arith_match_interp() {
    const A: u128 = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
    const B: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    const D: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(eax, 0b1010i32).unwrap();
            a.kmovw(k1, eax).unwrap();
            a.mov(eax, 0xFFFFi32).unwrap();
            a.kmovw(k2, eax).unwrap();
            a.xor(eax, eax).unwrap();
            a.kmovw(k3, eax).unwrap();
            a.vpaddd(xmm0.k1(), xmm1, xmm2).unwrap(); // merge, dword granularity
            a.vpsubd(xmm3.k1().z(), xmm1, xmm2).unwrap(); // zero
            a.vpaddq(xmm4.k2(), xmm1, xmm2).unwrap(); // all-ones merge = full write, qword
            a.vpaddd(xmm5.k3(), xmm1, xmm2).unwrap(); // all-zero merge = dst unchanged
            a.vpaddd(zmm6.k1().z(), zmm1, zmm2).unwrap(); // 512-bit zero-masked
            a.hlt().unwrap();
        },
        |c| {
            for r in [1, 2] {
                c.xmm[r] = if r == 1 { A } else { B };
                c.ymm_hi[r] = if r == 1 { B } else { A };
                c.zmm_hi[r] = if r == 1 { [A, B] } else { [B, A] };
            }
            for r in [0, 3, 4, 5, 6] {
                c.xmm[r] = D;
            }
            c.zmm_hi[6] = [D, D];
            c.ymm_hi[6] = D;
        },
        &[],
    );
}

/// EVEX scalar `vrndscale{sd,ss}` with scale factor M=0 (task-195): a 3-operand
/// `round{sd,ss}` — the low element is rounded under imm8[1:0], the upper bits come from
/// op1, bits 255:128 clear. imm8=0x01 (round down) and 0x02 (round up). JIT == interp.
#[test]
fn avx512_vrndscale_scalar_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vrndscalesd(xmm0, xmm1, xmm2, 0x01).unwrap(); // floor(double)
            a.vrndscalesd(xmm3, xmm1, xmm2, 0x02).unwrap(); // ceil(double)
            a.vrndscaless(xmm4, xmm1, xmm2, 0x03).unwrap(); // trunc(single)
            a.hlt().unwrap();
        },
        |c| {
            // low double = 13.7 in xmm2; low single = -2.9 in xmm1's low 32.
            c.xmm[2] = (13.7f64).to_bits() as u128;
            c.xmm[1] = (-2.9f32).to_bits() as u128;
        },
        &[],
    );
}

/// VEX.128 helpers the coreutils corpus hits (task-195): `vpunpcklqdq` interleave,
/// `vpsrldq` whole-lane byte shift, and 3-operand `vcvtsd2ss` — each clears bits 255:128.
/// The ymm-high dirtying proves the upper-zeroing. JIT == interp.
#[test]
fn vex_unpack_byteshift_cvt_match_interp() {
    const A: u128 = 0x1122_3344_5566_7788_99AA_BBCC_DDEE_FF00;
    const B: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpunpcklqdq(xmm0, xmm1, xmm2).unwrap();
            a.vpsrldq(xmm3, xmm1, 5).unwrap();
            a.vpslldq(xmm4, xmm2, 3).unwrap();
            a.vcvtsd2ss(xmm5, xmm1, xmm2).unwrap();
            a.vcvtss2sd(xmm6, xmm2, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = A;
            c.xmm[2] = B;
            // Dirty the ymm-high of every dst so the VEX upper-zeroing is observable.
            for r in [0, 3, 4, 5, 6] {
                c.ymm_hi[r] = u128::MAX;
            }
        },
        &[],
    );
}

/// task-202: 3-operand VEX scalar/packed float ops where op2 aliases the destination —
/// `vaddsd xmm0, xmm1, xmm0` and friends. The lift must not pre-copy op1 into dst (that
/// clobbers op2 before it is read). Covers commutative (add/mul/min/max) and
/// non-commutative (sub/div) ops in both register and memory-source forms. This is the
/// shape CPython 3.14's `_PyLong_Frexp` emits, behind the `float(2**30)==0.0` bug.
#[test]
fn vex_float_bin_dst_aliases_src2_match_interp() {
    const P: u128 = 0x4008_0000_0000_0000; // 3.0
    const Q: u128 = 0x4014_0000_0000_0000; // 5.0
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // Register op2 == dst (the aliasing case that regressed).
            a.vaddsd(xmm0, xmm1, xmm0).unwrap(); // xmm0 = xmm1 + xmm0
            a.vsubsd(xmm2, xmm1, xmm2).unwrap(); // xmm2 = xmm1 - xmm2 (order matters)
            a.vmulsd(xmm3, xmm1, xmm3).unwrap();
            a.vdivss(xmm4, xmm1, xmm4).unwrap();
            a.vminsd(xmm5, xmm1, xmm5).unwrap();
            a.vmaxsd(xmm6, xmm1, xmm6).unwrap();
            a.vsubps(xmm7, xmm1, xmm7).unwrap(); // packed, dst == src2
                                                 // Memory op2 (can't alias a register) — the branch that keeps the pre-copy.
            a.vmovsd(qword_ptr(SCRATCH), xmm0).unwrap(); // stage a double in memory
            a.vaddsd(xmm8, xmm1, qword_ptr(SCRATCH)).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = P;
            for r in [0, 2, 3, 4, 5, 6, 7, 8] {
                c.xmm[r] = Q;
                c.ymm_hi[r] = u128::MAX; // VEX upper-zeroing observable
            }
        },
        &[],
    );
}

/// task-203: the rest of the VEX 3-operand `op2==dst` aliasing family (siblings of the
/// task-202 vaddsd bug) — in-place ops that previously pre-copied op1 into dst and so
/// clobbered a register op2 aliasing dst: `vpshufb`, `vpalignr`, `vroundsd`, `vsqrtsd`,
/// `vmovsd`. Each now carries an explicit source in its IR op. Register op2 == dst below;
/// native cross-check (`native_vex_alias_family_*`) validates the semantics against the CPU.
#[test]
fn vex_alias_family_dst_aliases_src2_match_interp() {
    const DATA: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    const CTRL: u128 = 0x8080_8080_0001_0203_0405_0607_0809_0a0b; // shuffle control
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpshufb(xmm0, xmm1, xmm0).unwrap(); // shuffle op1 by control==dst
            a.vpalignr(xmm2, xmm1, xmm2, 5).unwrap(); // concat op1:op2, op2==dst
            a.vrndscalesd(xmm3, xmm1, xmm3, 1).unwrap(); // EVEX round op2==dst, merge op1
            a.vsqrtsd(xmm4, xmm1, xmm4).unwrap(); // sqrt op2==dst, merge op1
            a.db(&[0xc5, 0xf3, 0x10, 0xed]).unwrap(); // vmovsd xmm5,xmm1,xmm5 (no 3-op asm)
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[1] = DATA;
            c.xmm[0] = CTRL;
            for r in [2, 3, 4, 5] {
                c.xmm[r] = 0x4014_0000_0000_0000; // 5.0 in low lane (for round/sqrt/mov)
            }
            for r in [0, 2, 3, 4, 5] {
                c.ymm_hi[r] = u128::MAX; // VEX upper-zeroing observable
            }
        },
        &[],
    );
}

/// Memory-source `pcmpistri` (task-195): `pcmpistri xmm, [mem], imm` — the loaded 128-bit
/// operand is compared against xmm0, ECX gets the index and the flags are set. Staged
/// through SCRATCH (store xmm2, then compare against it); the register form is included as
/// a cross-check. imm=0x0C = equal-each, byte, least-significant index.
#[test]
fn pcmpistri_mem_src_match_interp() {
    // "hello\0..." in xmm0; a needle set in xmm2.
    const HAY: u128 = 0x0000_0000_0000_0000_0000_006F_6C6C_6568; // "hello"
    const NEEDLE: u128 = 0x0000_0000_0000_0000_0000_0000_0000_6C6C; // "ll"
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.movdqu(xmmword_ptr(SCRATCH), xmm2).unwrap(); // stage src2 in memory
            a.pcmpistri(xmm0, xmmword_ptr(SCRATCH), 0x0C).unwrap(); // mem form → ECX
            a.mov(dword_ptr(SCRATCH + 32), ecx).unwrap();
            a.pcmpistri(xmm0, xmm2, 0x0C).unwrap(); // register form (cross-check)
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = HAY;
            c.xmm[2] = NEEDLE;
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

/// SSE4.2 `pcmpistrm`/`pcmpestrm` (task-195): the string-aggregation result written as a
/// MASK to XMM0 (byte-mask via imm[6]=1, bit-mask via imm[6]=0), plus flags — JIT == interp
/// (both route through the shared pcmpstrm helper). The memory-source form is exercised too.
#[test]
fn sse42_pcmpstrm_match_interp() {
    const S1: u128 = 0x00_00_6F_6C_6C_65_48_64_6C_72_6F_77_20_6F_6C_6C; // mixed bytes + nulls
    const S2: u128 = 0x00_00_00_00_00_00_00_00_6C_72_6F_77_20_6F_6C_6C;
    jit_eq_interp(
        |a| {
            // Byte-mask form (imm[6]=1): each result bit → a full 0x00/0xFF byte in XMM0.
            a.pcmpistrm(xmm0, xmm1, 0x4C).unwrap(); // equal-ordered, byte mask
            a.movdqu(xmmword_ptr(SCRATCH), xmm0).unwrap(); // stash the mask before it's clobbered
            a.setb(r8b).unwrap();
            a.seto(r9b).unwrap();
            // Bit-mask form (imm[6]=0): result bits packed into the low bytes of XMM0.
            a.pcmpistrm(xmm0, xmm1, 0x18).unwrap(); // equal-each, bit mask
            a.setb(r10b).unwrap();
            // Memory-source + explicit-length pcmpestrm (bit mask).
            a.movdqu(xmmword_ptr(SCRATCH + 16), xmm1).unwrap();
            a.mov(eax, 6).unwrap();
            a.mov(edx, 8).unwrap();
            a.pcmpestrm(xmm0, xmmword_ptr(SCRATCH + 16), 0x0C).unwrap();
            a.sets(r11b).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = S1;
            c.xmm[1] = S2;
        },
        &[],
    );
}

/// SSE4.1 `insertps` (task-195): insert a src dword into a dst lane and zero lanes via
/// imm[3:0] — JIT == interp. Covers register (with a source-lane select + zeroing) and the
/// m32 memory form. imm=0x4E: src lane 1 → dst lane 0, zero dword 3 (imm[3:0]=0b1000 →
/// actually 0xE low nibble zeroes dwords 1,2,3). A pure-zeroing imm and a no-zero imm too.
#[test]
fn sse41_insertps_match_interp() {
    let f32x4 = |a: f32, b: f32, c: f32, d: f32| {
        (a.to_bits() as u128)
            | ((b.to_bits() as u128) << 32)
            | ((c.to_bits() as u128) << 64)
            | ((d.to_bits() as u128) << 96)
    };
    jit_eq_interp(
        |a| {
            a.insertps(xmm0, xmm1, 0x4E).unwrap(); // src lane1 → dst lane0, zero lanes 1-3
            a.insertps(xmm2, xmm3, 0xA0).unwrap(); // src lane2 → dst lane2, no zeroing
            a.insertps(xmm4, xmm5, 0x0F).unwrap(); // insert dst lane0, then zero ALL dwords
            a.movd(dword_ptr(SCRATCH), xmm6).unwrap(); // stage a dword in memory for the m32 form
            a.insertps(xmm7, dword_ptr(SCRATCH), 0x10).unwrap(); // m32 → dst lane1, no zero
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = f32x4(1.0, 2.0, 3.0, 4.0);
            c.xmm[1] = f32x4(10.0, 20.0, 30.0, 40.0);
            c.xmm[2] = f32x4(5.0, 6.0, 7.0, 8.0);
            c.xmm[3] = f32x4(50.0, 60.0, 70.0, 80.0);
            c.xmm[4] = f32x4(1.5, 2.5, 3.5, 4.5);
            c.xmm[5] = f32x4(9.0, 9.0, 9.0, 9.0);
            c.xmm[6] = f32x4(123.0, 0.0, 0.0, 0.0); // dword0 = 123.0 for the m32 load
            c.xmm[7] = f32x4(-1.0, -2.0, -3.0, -4.0);
        },
        &[],
    );
}

/// SSE4.1 `dpps` (task-195): single-precision dot product with a partial product mask and a
/// NaN lane (so NaN propagation + f32 rounding are exercised) — JIT == interp (both route
/// through the shared dpps helper). Register and m128 memory forms.
#[test]
fn sse41_dpps_match_interp() {
    let f32x4 = |a: f32, b: f32, c: f32, d: f32| {
        (a.to_bits() as u128)
            | ((b.to_bits() as u128) << 32)
            | ((c.to_bits() as u128) << 64)
            | ((d.to_bits() as u128) << 96)
    };
    jit_eq_interp(
        |a| {
            // imm=0x71: include products 0..3? high nibble 0x7 = lanes 0,1,2; low nibble 0x1
            // = broadcast sum to dword 0 only. Lane 3 (NaN) is excluded from the sum.
            a.dpps(xmm0, xmm1, 0x71).unwrap();
            // imm=0xF5: all 4 products (incl. the NaN lane) → NaN sum; broadcast to dwords 0,2.
            a.dpps(xmm2, xmm3, 0xF5).unwrap();
            // Memory form.
            a.movdqu(xmmword_ptr(SCRATCH), xmm5).unwrap();
            a.dpps(xmm4, xmmword_ptr(SCRATCH), 0x31).unwrap();
            // imm=0xFF: every product summed, broadcast to every lane (task-237 native path).
            a.dpps(xmm6, xmm7, 0xFF).unwrap();
            // imm=0x88: input lane 3 only, output lane 3 only (high-lane insert, others 0).
            a.dpps(xmm8, xmm9, 0x88).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = f32x4(1.0, 2.0, 3.0, 4.0);
            c.xmm[1] = f32x4(0.5, 0.25, 2.0, 100.0);
            c.xmm[2] = f32x4(1.0, f32::NAN, 3.0, 4.0);
            c.xmm[3] = f32x4(1.0, 1.0, 1.0, 1.0);
            c.xmm[4] = f32x4(2.0, 3.0, 4.0, 5.0);
            c.xmm[5] = f32x4(1.5, 2.5, 3.5, 4.5);
            c.xmm[6] = f32x4(1.0, 2.0, 3.0, 4.0);
            c.xmm[7] = f32x4(10.0, 20.0, 30.0, 40.0);
            c.xmm[8] = f32x4(7.0, 8.0, 9.0, 10.0);
            c.xmm[9] = f32x4(0.5, 0.25, 0.125, 6.0);
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

/// AES-NI SSE + VEX (task-205): every op (aesenc/dec/enclast/declast/imc/keygen) plus a
/// VEX 3-operand form, register and memory sources. JIT must match the interpreter; the
/// VEX forms must zero bits 255:128 (ymm-high seeded dirty to prove it).
#[test]
fn aes_all_variants_match_interp() {
    const S: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    const K: u128 = 0x1032_5476_98ba_dcfe_efcd_ab89_6745_2301;
    const DIRTY: u128 = 0xdead_beef_cafe_babe_0bad_f00d_feed_face;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // SSE in-place forms (register key).
            a.aesenc(xmm0, xmm1).unwrap();
            a.aesdec(xmm2, xmm1).unwrap();
            a.aesenclast(xmm3, xmm1).unwrap();
            a.aesdeclast(xmm4, xmm1).unwrap();
            a.aesimc(xmm5, xmm1).unwrap();
            a.aeskeygenassist(xmm6, xmm1, 0x1b).unwrap();
            // SSE memory-key form.
            a.movdqu(xmmword_ptr(SCRATCH), xmm1).unwrap();
            a.aesenc(xmm7, xmmword_ptr(SCRATCH)).unwrap();
            // VEX.128 3-operand forms (dst distinct; must zero 255:128).
            a.vaesenc(xmm8, xmm1, xmm2).unwrap();
            a.vaesdec(xmm9, xmm1, xmm2).unwrap();
            a.vaesenclast(xmm10, xmm1, xmm2).unwrap();
            a.vaesdeclast(xmm11, xmm1, xmm2).unwrap();
            a.vaesimc(xmm12, xmm1).unwrap();
            a.vaeskeygenassist(xmm13, xmm1, 0x2a).unwrap();
            // VEX memory-key form.
            a.vaesenc(xmm14, xmm1, xmmword_ptr(SCRATCH)).unwrap();
            // VEX dst aliasing the key source must not clobber early (dst==key reg).
            a.vaesenc(xmm2, xmm1, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for r in 0..=14 {
                c.xmm[r] = S ^ ((r as u128) << 8);
                c.ymm_hi[r] = DIRTY; // dirty upper — VEX forms must clear it
            }
            c.xmm[1] = K;
        },
        &[],
    );
}

/// SHA-NI SSE (task-207): every op (sha256rnds2/msg1/msg2, sha1rnds4/nexte/msg1/msg2),
/// register and memory second-source forms. `sha256rnds2` reads xmm0 implicitly, so xmm0
/// is seeded distinctly. `sha1rnds4` is exercised with all four `imm8[1:0]` functions.
/// JIT must match the interpreter bit-for-bit.
#[test]
fn sha_all_variants_match_interp() {
    const S: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    const WK: u128 = 0x1032_5476_98ba_dcfe_efcd_ab89_6745_2301;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // SHA-256 (register second source; sha256rnds2 uses xmm0 implicitly).
            a.sha256rnds2(xmm1, xmm2).unwrap();
            a.sha256msg1(xmm3, xmm2).unwrap();
            a.sha256msg2(xmm4, xmm2).unwrap();
            // SHA-1 (all four imm-selected round functions).
            a.sha1rnds4(xmm5, xmm2, 0u32).unwrap();
            a.sha1rnds4(xmm6, xmm2, 1u32).unwrap();
            a.sha1rnds4(xmm7, xmm2, 2u32).unwrap();
            a.sha1rnds4(xmm8, xmm2, 3u32).unwrap();
            a.sha1nexte(xmm9, xmm2).unwrap();
            a.sha1msg1(xmm10, xmm2).unwrap();
            a.sha1msg2(xmm11, xmm2).unwrap();
            // Memory second-source forms.
            a.movdqu(xmmword_ptr(SCRATCH), xmm2).unwrap();
            a.sha256rnds2(xmm12, xmmword_ptr(SCRATCH)).unwrap();
            a.sha1rnds4(xmm13, xmmword_ptr(SCRATCH), 2u32).unwrap();
            a.sha1msg1(xmm14, xmmword_ptr(SCRATCH)).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for r in 0..=14 {
                c.xmm[r] = S ^ ((r as u128) << 8);
            }
            c.xmm[0] = WK; // implicit W+K operand for sha256rnds2
            c.xmm[2] = WK ^ 0x55; // second source
        },
        &[],
    );
}

/// SSSE3 `psign{b,w,d}` + VEX.128 `vpsign{b,w,d}` (task-210): per-element negate/zero/keep
/// by the sign of the control operand, all three widths, register + memory second source.
/// The VEX forms must zero bits 255:128 (upper half seeded dirty). Ctrl values are chosen
/// so each lane hits all three cases (negative / zero / positive). JIT must match interp.
#[test]
fn psign_all_variants_match_interp() {
    const SRC: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    // Mix of negative (high bit set), zero, and positive lanes across widths.
    const CTRL: u128 = 0x8000_00ff_ff00_0080_007f_0000_ff80_0001;
    const DIRTY: u128 = 0xdead_beef_cafe_babe_0bad_f00d_feed_face;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // SSE in-place forms (register ctrl in xmm1).
            a.psignb(xmm0, xmm1).unwrap();
            a.psignw(xmm2, xmm1).unwrap();
            a.psignd(xmm3, xmm1).unwrap();
            // SSE memory-ctrl form.
            a.movdqu(xmmword_ptr(SCRATCH), xmm1).unwrap();
            a.psignb(xmm4, xmmword_ptr(SCRATCH)).unwrap();
            // VEX.128 3-operand forms (dst distinct; must zero 255:128).
            a.vpsignb(xmm8, xmm5, xmm1).unwrap();
            a.vpsignw(xmm9, xmm5, xmm1).unwrap();
            a.vpsignd(xmm10, xmm5, xmm1).unwrap();
            // VEX memory-ctrl form.
            a.vpsignd(xmm11, xmm5, xmmword_ptr(SCRATCH)).unwrap();
            // VEX dst aliasing the ctrl source must not clobber early (dst==ctrl reg).
            a.vpsignb(xmm1, xmm5, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for r in 0..=11 {
                c.xmm[r] = SRC ^ ((r as u128) << 8);
                c.ymm_hi[r] = DIRTY; // dirty upper — VEX forms must clear it
            }
            c.xmm[1] = CTRL; // control operand
            c.xmm[5] = SRC ^ 0x1234; // VEX op1 source
        },
        &[],
    );
}

/// GFNI `gf2p8mulb/gf2p8affineqb/gf2p8affineinvqb` (SSE) + VEX.128 `vgf2p8*` (task-210),
/// register + memory second source, affine imm8 exercised. The VEX forms must zero bits
/// 255:128 (upper half seeded dirty). JIT must match interp bit-for-bit.
#[test]
fn gfni_all_variants_match_interp() {
    const X: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    const M: u128 = 0x1032_5476_98ba_dcfe_efcd_ab89_6745_2301;
    const DIRTY: u128 = 0xdead_beef_cafe_babe_0bad_f00d_feed_face;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // SSE in-place forms (register second source in xmm1).
            a.gf2p8mulb(xmm0, xmm1).unwrap();
            a.gf2p8affineqb(xmm2, xmm1, 0x5au32).unwrap();
            a.gf2p8affineinvqb(xmm3, xmm1, 0xa5u32).unwrap();
            // SSE memory second-source form.
            a.movdqu(xmmword_ptr(SCRATCH), xmm1).unwrap();
            a.gf2p8mulb(xmm4, xmmword_ptr(SCRATCH)).unwrap();
            a.gf2p8affineqb(xmm5, xmmword_ptr(SCRATCH), 0x11u32)
                .unwrap();
            // VEX.128 3-operand forms (dst distinct; must zero 255:128).
            a.vgf2p8mulb(xmm8, xmm6, xmm1).unwrap();
            a.vgf2p8affineqb(xmm9, xmm6, xmm1, 0x3cu32).unwrap();
            a.vgf2p8affineinvqb(xmm10, xmm6, xmm1, 0xc3u32).unwrap();
            // VEX memory second-source form.
            a.vgf2p8mulb(xmm11, xmm6, xmmword_ptr(SCRATCH)).unwrap();
            // VEX dst aliasing the second source must not clobber early (dst==src reg).
            a.vgf2p8mulb(xmm1, xmm6, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for r in 0..=11 {
                c.xmm[r] = X ^ ((r as u128) << 8);
                c.ymm_hi[r] = DIRTY; // dirty upper — VEX forms must clear it
            }
            c.xmm[1] = M; // second source (multiplier / affine matrix)
            c.xmm[6] = X ^ 0x77; // VEX op1 source
        },
        &[],
    );
}

/// PCLMULQDQ `pclmulqdq` (SSE) + VEX.128 `vpclmulqdq` (task-211), register + memory second
/// source, all four imm8 half-selections. The VEX forms must zero bits 255:128 (upper half
/// seeded dirty). JIT must match interp bit-for-bit.
#[test]
fn pclmul_all_variants_match_interp() {
    const A: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
    const B: u128 = 0x1032_5476_98ba_dcfe_efcd_ab89_6745_2301;
    const DIRTY: u128 = 0xdead_beef_cafe_babe_0bad_f00d_feed_face;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // SSE in-place forms: all four imm8 half-selections (register op2 in xmm1).
            a.pclmulqdq(xmm0, xmm1, 0x00).unwrap();
            a.pclmulqdq(xmm2, xmm1, 0x01).unwrap();
            a.pclmulqdq(xmm3, xmm1, 0x10).unwrap();
            a.pclmulqdq(xmm4, xmm1, 0x11).unwrap();
            // SSE memory second-source form.
            a.movdqu(xmmword_ptr(SCRATCH), xmm1).unwrap();
            a.pclmulqdq(xmm5, xmmword_ptr(SCRATCH), 0x11).unwrap();
            // VEX.128 3-operand forms (dst distinct; must zero 255:128).
            a.vpclmulqdq(xmm8, xmm6, xmm1, 0x00).unwrap();
            a.vpclmulqdq(xmm9, xmm6, xmm1, 0x11).unwrap();
            // VEX memory second-source form.
            a.vpclmulqdq(xmm10, xmm6, xmmword_ptr(SCRATCH), 0x01)
                .unwrap();
            // VEX dst aliasing the second source must not clobber early (dst==src reg).
            a.vpclmulqdq(xmm1, xmm6, xmm1, 0x10).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for r in 0..=10 {
                c.xmm[r] = A ^ ((r as u128) << 8);
                c.ymm_hi[r] = DIRTY; // dirty upper — VEX forms must clear it
            }
            c.xmm[1] = B; // second source (op2)
            c.xmm[6] = A ^ 0x77; // VEX op1 source
        },
        &[],
    );
}

/// task-215: EVEX-512 packed shift-by-imm at ZMM width, unmasked + merge/zeroing masked
/// (the openssl-genrsa trap chain). JIT (helper) must match interp bit-for-bit.
#[test]
fn masked_shift_512_match_interp() {
    const A: u128 = 0x8001_7FFF_1234_5678_FFFF_0000_ABCD_1234;
    const A_HI: u128 = 0x0FF0_1234_DEAD_BEEF_8000_0001_9999_0000;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpsrld(zmm3, zmm0, 0x1fu32).unwrap(); // the exact genrsa trap
            a.vpslld(zmm4, zmm0, 3u32).unwrap();
            a.vpsrad(zmm5, zmm0, 5u32).unwrap();
            a.vpsrlq(zmm6, zmm0, 17u32).unwrap();
            a.vpsraq(zmm7, zmm0, 63u32).unwrap();
            a.mov(eax, 0x0000_cc33u32).unwrap();
            a.kmovd(k1, eax).unwrap();
            a.vmovdqa64(zmm8, zmm1).unwrap();
            a.vpsrld(zmm8.k1(), zmm0, 4u32).unwrap(); // merge
            a.vpslld(zmm9.k1().z(), zmm0, 6u32).unwrap(); // zeroing
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = A_HI;
            c.zmm_hi[0] = [A ^ 0x55, A_HI ^ 0xaa];
            c.xmm[1] = 0xEEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE; // merge base
            c.ymm_hi[1] = 0xEEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE;
            c.zmm_hi[1] = [0xEEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE_EEEE; 2];
        },
        &[],
    );
}

/// task-215: `pmuludq`/`vpmuludq` unsigned low-dword → 64-bit product, SSE + VEX.128/256,
/// register and memory second source. JIT (inline mask+imul) must match interp.
#[test]
fn vpmuludq_match_interp() {
    const A: u128 = 0xFFFF_FFFF_1234_5678_DEAD_BEEF_9ABC_DEF0;
    const B: u128 = 0x8765_4321_FEDC_BA98_1111_2222_3333_4444;
    const A_HI: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    const B_HI: u128 = 0x1020_3040_5060_7080_90A0_B0C0_D0E0_F000;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.movdqa(xmm2, xmm0).unwrap();
            a.pmuludq(xmm2, xmm1).unwrap(); // SSE in-place
            a.vpmuludq(xmm3, xmm0, xmm1).unwrap(); // VEX.128 reg
            a.vmovdqu(xmmword_ptr(SCRATCH), xmm1).unwrap();
            a.vpmuludq(xmm4, xmm0, xmmword_ptr(SCRATCH)).unwrap(); // VEX.128 mem
            a.vpmuludq(ymm5, ymm0, ymm1).unwrap(); // VEX.256 reg (genrsa form)
            a.vmovdqu(ymmword_ptr(SCRATCH + 32), ymm1).unwrap();
            a.vpmuludq(ymm6, ymm0, ymmword_ptr(SCRATCH + 32)).unwrap(); // VEX.256 mem
            a.vpmuludq(zmm7, zmm0, zmm1).unwrap(); // EVEX.512 reg (RSA-2048 montgomery)
            a.vmovdqu64(zmmword_ptr(SCRATCH + 64), zmm1).unwrap();
            a.vpmuludq(zmm8, zmm0, zmmword_ptr(SCRATCH + 64)).unwrap(); // EVEX.512 mem
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = A_HI;
            c.zmm_hi[0] = [A ^ 0x1234, A_HI ^ 0x5678];
            c.xmm[1] = B;
            c.ymm_hi[1] = B_HI;
            c.zmm_hi[1] = [B ^ 0x9abc, B_HI ^ 0xdef0];
        },
        &[],
    );
}

/// task-215: memory-source single-table permute `vperm{q,d} v, idx, [mem]` (EVEX-512,
/// genrsa-1024 trap), unmasked + merge/zeroing. JIT (fault-capable helper) == interp.
#[test]
fn vperm1_mem_match_interp() {
    const IDX: u128 = 0x0000_0003_0000_0006_0000_0001_0000_0004;
    const IDX_HI: u128 = 0x0000_0000_0000_0007_0000_0005_0000_0002;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu64(zmmword_ptr(rax), zmm1).unwrap(); // stage the table at [scratch]
            a.vpermq(zmm2, zmm0, zmmword_ptr(rax)).unwrap(); // qword gather (idx=zmm0)
            a.vpermd(zmm3, zmm0, zmmword_ptr(rax)).unwrap(); // dword gather
            a.mov(ecx, 0x0000_a5c3u32).unwrap();
            a.kmovd(k1, ecx).unwrap();
            a.vmovdqa64(zmm4, zmm5).unwrap();
            a.vpermq(zmm4.k1(), zmm0, zmmword_ptr(rax)).unwrap(); // merge
            a.vpermd(zmm6.k1().z(), zmm0, zmmword_ptr(rax)).unwrap(); // zeroing
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = IDX;
            c.ymm_hi[0] = IDX_HI;
            c.zmm_hi[0] = [IDX ^ 0x0000_0002_0000_0000, IDX_HI]; // more index lanes
            let t: u128 = 0x1111_2222_3333_4444_5555_6666_7777_8888;
            c.xmm[1] = t;
            c.ymm_hi[1] = t ^ 0xffff;
            c.zmm_hi[1] = [t ^ 0xaaaa, t ^ 0x5555];
            c.xmm[5] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF; // merge base
            c.ymm_hi[5] = 0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF;
            c.zmm_hi[5] = [0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEF; 2];
        },
        &[],
    );
}

/// task-215: `vpblendd` per-dword immediate blend, VEX.128 + VEX.256. JIT (byte shuffle)
/// must match interp.
#[test]
fn vpblendd_match_interp() {
    const A: u128 = 0x1111_1111_2222_2222_3333_3333_4444_4444;
    const B: u128 = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
    const A_HI: u128 = 0x5555_5555_6666_6666_7777_7777_8888_8888;
    const B_HI: u128 = 0xEEEE_EEEE_FFFF_FFFF_0000_0000_9999_9999;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpblendd(xmm2, xmm0, xmm1, 0x3).unwrap();
            a.vpblendd(xmm3, xmm0, xmm1, 0xa).unwrap();
            a.vpblendd(ymm4, ymm0, ymm1, 0x3).unwrap(); // genrsa form
            a.vpblendd(ymm5, ymm0, ymm1, 0x5a).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = A_HI;
            c.xmm[1] = B;
            c.ymm_hi[1] = B_HI;
        },
        &[],
    );
}

/// task-215: `vzeroall` zeros the whole vector file (low 128 + uppers); `vzeroupper`
/// zeros only the uppers, preserving the low 128. The JIT emit must match the interp
/// for both — a prior bug cleared only the uppers for `vzeroall`, leaving xmm stale.
#[test]
fn vzeroall_vs_vzeroupper_low_lane() {
    // vzeroall clears everything.
    jit_eq_interp(
        |a| {
            a.vzeroall().unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for i in 0..16 {
                c.xmm[i] = 0x1111_2222_3333_4444_5555_6666_7777_8888 ^ (i as u128);
                c.ymm_hi[i] = 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000 ^ (i as u128);
            }
        },
        &[],
    );
    // vzeroupper clears uppers only, preserves the low 128.
    jit_eq_interp(
        |a| {
            a.vzeroupper().unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for i in 0..16 {
                c.xmm[i] = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef ^ (i as u128);
                c.ymm_hi[i] = 0xfedc_ba98_7654_3210_fedc_ba98_7654_3210 ^ (i as u128);
            }
        },
        &[],
    );
}

/// task-221: `vzeroupper` under AVX-512 must zero bits 511:128 of ZMM0–15 — including
/// `zmm_hi` (511:256), not just `ymm_hi` (255:128). A prior JIT bug cleared only
/// `ymm_hi`, leaving `zmm_hi` stale → jit != interp when the zmm uppers were live.
#[test]
fn vzeroupper_clears_zmm_hi() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vzeroupper().unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for i in 0..16 {
                c.xmm[i] = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff ^ (i as u128);
                c.ymm_hi[i] = 0xfeed_face_cafe_babe_dead_beef_0bad_f00d ^ (i as u128);
                // Live zmm upper 256 bits — must be zeroed by vzeroupper.
                c.zmm_hi[i][0] = 0x1234_5678_9abc_def0_0f0e_0d0c_0b0a_0908 ^ (i as u128);
                c.zmm_hi[i][1] = 0xa5a5_5a5a_c3c3_3c3c_9696_6969_0f0f_f0f0 ^ (i as u128);
            }
        },
        &[],
    );
    // vzeroall additionally clears the low 128 bits.
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vzeroall().unwrap();
            a.hlt().unwrap();
        },
        |c| {
            for i in 0..16 {
                c.xmm[i] = 0x1111_2222_3333_4444_5555_6666_7777_8888 ^ (i as u128);
                c.ymm_hi[i] = 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000 ^ (i as u128);
                c.zmm_hi[i][0] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210 ^ (i as u128);
                c.zmm_hi[i][1] = 0xdead_c0de_face_feed_b105_f00d_1dea_babe ^ (i as u128);
            }
        },
        &[],
    );
}

/// task-221: `pinsrw`/`vpinsrw` insert a 16-bit word into an xmm lane. A prior JIT bug
/// used the wrong vector type (I64X2) for size 2, so `insertlane` got a lane index up
/// to 7 on a 2-lane vector and the Cranelift verifier rejected the block on tier-up.
/// The correct lane type is I16X8; assert jit==interp across several word lanes.
#[test]
fn pinsrw_match_interp() {
    const BASE: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    for lane in 0..8u32 {
        // pinsrw xmm, r32, imm (SSE2)
        jit_eq_interp(
            |a| {
                a.pinsrw(xmm1, eax, lane).unwrap();
                a.hlt().unwrap();
            },
            |c| {
                c.xmm[1] = BASE;
                c.gpr[Reg::Rax as usize] = 0xdead_beef;
            },
            &[],
        );
        // vpinsrw xmm, xmm, r32, imm (VEX.128) — three-operand form.
        jit_eq_interp(
            |a| {
                a.vpinsrw(xmm2, xmm1, eax, lane).unwrap();
                a.hlt().unwrap();
            },
            |c| {
                c.xmm[1] = BASE;
                c.gpr[Reg::Rax as usize] = 0xcafe_1234;
            },
            &[],
        );
    }
}

/// task-168.6: `vextractps r/m32, xmm, imm8` (VEX.128.66.0F3A.W0 17) extracts the
/// 32-bit float lane `imm8[1:0]` to a GPR32 (upper 32 bits zeroed) or a memory dword.
/// The JIT must match interp for both dst forms across all four lanes (interp is in
/// turn oracle-validated against Unicorn in differential.rs). The mem-dst form is the
/// exact shape that walled Celeste boot (`vextractps $0x2,%xmm0,0x2c(%rsp)`).
#[test]
fn vextractps_match_interp() {
    const SRC: u128 = 0xDDDD_DDDD_CCCC_CCCC_BBBB_BBBB_AAAA_AAAA;
    for lane in 0..4u32 {
        // reg32 dst — seed with all-ones so the upper-32 zero-extend is observable.
        jit_eq_interp(
            |a| {
                a.vextractps(eax, xmm0, lane as i32).unwrap();
                a.hlt().unwrap();
            },
            |c| {
                c.xmm[0] = SRC;
                c.gpr[Reg::Rax as usize] = 0xFFFF_FFFF_FFFF_FFFF;
            },
            &[],
        );
        // mem32 dst — store the lane, read it back so the JIT/interp state diverges on a
        // bug. A sentinel in the adjacent dword, read back into ecx, pins the store to
        // exactly 4 bytes (an 8-byte store would clobber SCRATCH+4).
        jit_eq_interp(
            |a| {
                a.mov(dword_ptr(SCRATCH + 4), 0x5A5A_5A5Au32 as i32)
                    .unwrap();
                a.vextractps(dword_ptr(SCRATCH), xmm0, lane as i32).unwrap();
                a.mov(ebx, dword_ptr(SCRATCH)).unwrap();
                a.mov(ecx, dword_ptr(SCRATCH + 4)).unwrap();
                a.hlt().unwrap();
            },
            |c| {
                c.xmm[0] = SRC;
            },
            &[],
        );
    }
}

/// task-215: `vpermilps`/`vpermilpd` with imm8, VEX.128 — reg and memory source. Both
/// are in-lane single-source permutes lowered to the dword shuffle; assert jit==interp.
/// openssl's rsaz-avx2 keygen emits the memory-source `vpermilpd`.
#[test]
fn vpermil_imm_match_interp() {
    const A: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    // vpermilps xmm, xmm, imm (reg)
    jit_eq_interp(
        |a| {
            a.vpermilps(xmm2, xmm1, 0b00_01_10_11i32).unwrap();
            a.hlt().unwrap();
        },
        |c| c.xmm[1] = A,
        &[],
    );
    // vpermilpd xmm, xmm, imm (reg): swap the two doubles
    jit_eq_interp(
        |a| {
            a.vpermilpd(xmm3, xmm1, 0b01i32).unwrap();
            a.hlt().unwrap();
        },
        |c| c.xmm[1] = A,
        &[],
    );
    // vpermilpd xmm, [mem], imm — the rsaz keygen form (memory source).
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vpermilpd(xmm4, xmmword_ptr(rax), 0b10i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
    // vpermilps xmm, [mem], imm (memory source).
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vpermilps(xmm5, xmmword_ptr(rax), 0b11_00_11_00i32)
                .unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// task-215: 16-bit `movbe` (byte-swap move) memory store/load. `movbe` reuses the
/// Bswap IR op; the interp did a 32-bit swap for the 16-bit operand (real `bswap r16`
/// is undefined, but movbe needs a true 2-byte swap), corrupting openssl's base64/PEM
/// key decode -> wrong RSA signatures. The JIT already swapped 16 bits correctly, so
/// this jit==interp check would have caught the interp bug (no prior 16-bit movbe test).
#[test]
fn movbe16_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x1122_3344_5566_7788u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.movbe(cx, word_ptr(SCRATCH)).unwrap(); // 16-bit byte-swap load
            a.movbe(word_ptr(SCRATCH + 16), cx).unwrap(); // 16-bit byte-swap store
            a.movbe(edx, dword_ptr(SCRATCH)).unwrap(); // 32-bit form still correct
            a.movbe(dword_ptr(SCRATCH + 24), edx).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// task-215: `vpermq`/`vpermpd` imm8 with a MEMORY source (VEX.256) — the mem form
/// loads 256 bits then permutes the 4 qwords. openssl rsaz signing emits
/// `vpermq ymm,[mem],imm`. jit==interp for reg and mem source.
#[test]
fn vpermq_mem_imm_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            // Store 4 distinct qwords so every permutation is observable.
            a.mov(rcx, 0x1111_1111_1111_1111u64).unwrap();
            a.mov(qword_ptr(rax), rcx).unwrap();
            a.mov(rcx, 0x2222_2222_2222_2222u64).unwrap();
            a.mov(qword_ptr(rax + 8), rcx).unwrap();
            a.mov(rcx, 0x3333_3333_3333_3333u64).unwrap();
            a.mov(qword_ptr(rax + 16), rcx).unwrap();
            a.mov(rcx, 0x4444_4444_4444_4444u64).unwrap();
            a.mov(qword_ptr(rax + 24), rcx).unwrap();
            a.vpermq(ymm1, ymmword_ptr(rax), 0b00_01_10_11i32).unwrap(); // mem, reversed qwords
            a.vpermpd(ymm2, ymmword_ptr(rax), 0b11_10_01_00i32).unwrap(); // mem, identity
            a.vmovdqu(ymm3, ymmword_ptr(rax)).unwrap();
            a.vpermq(ymm4, ymm3, 0b01_00_11_10i32).unwrap(); // reg
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// task-215 (TLS): packed integer multiplies added for the openssl TLS handshake —
/// `pmullw`/`vpmullw` (low-16), `pmulhw`/`pmulhuw` (high-16 signed/unsigned),
/// `vpmulld` (VEX low-32), and `pmuldq`/`vpmuldq` (signed 32×32→64).
#[test]
fn packed_muls_match_interp() {
    const A: u128 = 0xFFFF_8001_1234_5678_DEAD_BEEF_9ABC_DEF0;
    const B: u128 = 0x8765_4321_FEDC_BA98_7FFF_0002_3333_4444;
    const A_HI: u128 = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10;
    const B_HI: u128 = 0x1020_3040_5060_7080_90A0_B0C0_D0E0_F000;
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.movdqa(xmm2, xmm0).unwrap();
            a.pmullw(xmm2, xmm1).unwrap(); // SSE low-16
            a.vpmullw(ymm3, ymm0, ymm1).unwrap(); // VEX.256 low-16
            a.movdqa(xmm4, xmm0).unwrap();
            a.pmulhw(xmm4, xmm1).unwrap(); // SSE high-16 signed
            a.vpmulhw(ymm5, ymm0, ymm1).unwrap();
            a.movdqa(xmm6, xmm0).unwrap();
            a.pmulhuw(xmm6, xmm1).unwrap(); // SSE high-16 unsigned
            a.vpmulhuw(ymm7, ymm0, ymm1).unwrap();
            a.vpmulld(ymm8, ymm0, ymm1).unwrap(); // VEX.256 low-32
            a.movdqa(xmm9, xmm0).unwrap();
            a.pmuldq(xmm9, xmm1).unwrap(); // SSE signed 32×32→64
            a.vpmuldq(zmm10, zmm0, zmm1).unwrap(); // EVEX.512 signed (openssl RSA)
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = A_HI;
            c.zmm_hi[0] = [A ^ 0x55, A_HI ^ 0xAA];
            c.xmm[1] = B;
            c.ymm_hi[1] = B_HI;
            c.zmm_hi[1] = [B ^ 0x33, B_HI ^ 0xCC];
        },
        &[],
    );
}

/// task-215 (TLS): per-element variable shifts `vp{sll,srl,sra}v{w,d,q}` and the
/// scalar-register-count shift `vpslld/vpsrad v,v,xmm`.
#[test]
fn variable_shifts_match_interp() {
    const A: u128 = 0x8000_0001_FEDC_BA98_7654_3210_0F1E_2D3C;
    const CNT: u128 = 0x0000_0021_0000_0003_0000_0011_0000_0007; // per-dword counts (incl over-shift)
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.vpsllvd(ymm2, ymm0, ymm1).unwrap();
            a.vpsrlvd(ymm3, ymm0, ymm1).unwrap();
            a.vpsravd(ymm4, ymm0, ymm1).unwrap();
            a.vpsllvq(ymm5, ymm0, ymm1).unwrap();
            a.vpsrlvq(ymm6, ymm0, ymm1).unwrap();
            a.vpsravq(zmm7, zmm0, zmm1).unwrap(); // EVEX-only arith qword variable
            a.vpsllvw(ymm12, ymm0, ymm1).unwrap(); // AVX-512BW word variable
            a.vpsrlvw(ymm13, ymm0, ymm1).unwrap();
            a.vpsravw(ymm14, ymm0, ymm1).unwrap();
            // Scalar register count (low 64 bits of the count xmm apply to all lanes).
            a.vpslld(ymm8, ymm0, xmm1).unwrap();
            a.vpsrld(ymm9, ymm0, xmm1).unwrap();
            a.vpsrad(ymm10, ymm0, xmm1).unwrap();
            a.vpsrlq(ymm11, ymm0, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = A;
            c.ymm_hi[0] = A.rotate_left(19);
            c.zmm_hi[0] = [A ^ 0x1234, A.rotate_left(7)];
            c.xmm[1] = CNT;
            c.ymm_hi[1] = CNT;
            c.zmm_hi[1] = [CNT, CNT];
        },
        &[],
    );
}

/// task-215 (TLS): `vpsrlq zmm,[mem],imm` — EVEX imm-shift with a memory source.
#[test]
fn shift_imm_mem_src_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu64(zmmword_ptr(rax), zmm0).unwrap();
            a.vpsrlq(zmm1, zmmword_ptr(rax), 9u32).unwrap();
            a.vpslld(zmm2, zmmword_ptr(rax), 5u32).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0xDEAD_BEEF_1234_5678_9ABC_DEF0_0F1E_2D3C;
            c.ymm_hi[0] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
            c.zmm_hi[0] = [0xAAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000_1111, 0x1234];
        },
        &[],
    );
}

/// task-215 (TLS): GFNI wide/masked `vgf2p8affineqb`/`vgf2p8mulb` on ymm/zmm, register
/// and rip-relative-memory matrix (openssl's vectorized AES).
#[test]
fn gfni_wide_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovdqu(ymmword_ptr(rax), ymm1).unwrap();
            a.vgf2p8affineqb(ymm2, ymm0, ymm1, 0x63u32).unwrap(); // reg matrix
            a.vgf2p8affineqb(ymm3, ymm0, ymmword_ptr(rax), 0x00u32)
                .unwrap(); // mem matrix, dst != src1
            a.vgf2p8mulb(ymm4, ymm0, ymm1).unwrap();
            a.vgf2p8affineqb(zmm5, zmm0, zmm1, 0x11u32).unwrap(); // EVEX.512
                                                                  // dst == src1 with a memory matrix (openssl AES form): the VGf2p8M path must
                                                                  // read the matrix from memory without clobbering the aliased source.
            a.vmovdqu(ymm6, ymm0).unwrap();
            a.vgf2p8affineqb(ymm6, ymm6, ymmword_ptr(rax), 0x1bu32)
                .unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0x0011_2233_4455_6677_8899_AABB_CCDD_EEFF;
            c.ymm_hi[0] = 0xFEDC_BA98_7654_3210_0123_4567_89AB_CDEF;
            c.zmm_hi[0] = [0xA5A5_5A5A_C3C3_3C3C_0F0F_F0F0_1234_5678, 0x99];
            c.xmm[1] = 0x1F_2E_3D_4C_5B_6A_79_88_97_A6_B5_C4_D3_E2_F1_00;
            c.ymm_hi[1] = 0x8040_2010_0804_0201_0102_0408_1020_4080;
            c.zmm_hi[1] = [0x0102_0408_1020_4080_8040_2010_0804_0201, 0x1];
        },
        &[],
    );
}

/// task-215 (caddy HTTPS): SSE4.1 `pblendw` — imm8 word blend, dst is also src1.
#[test]
fn pblendw_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.movdqa(xmm2, xmm0).unwrap();
            a.pblendw(xmm2, xmm1, 0xf0u32).unwrap(); // high 4 words from src2
            a.movdqa(xmm3, xmm0).unwrap();
            a.pblendw(xmm3, xmm1, 0xa5u32).unwrap(); // alternating pattern
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
            c.xmm[1] = 0xAAAA_BBBB_CCCC_DDDD_EEEE_FFFF_0000_9999;
        },
        &[],
    );
}

/// task-215 (TLS): VEX 4-operand variable blends and EVEX qword compare→mask.
#[test]
fn blend_and_cmpq_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            // vpblendvb/vblendvps: explicit mask register (xmm2 high bit per lane).
            a.vpblendvb(xmm3, xmm0, xmm1, xmm2).unwrap();
            a.vblendvps(xmm4, xmm0, xmm1, xmm2).unwrap();
            a.vblendvpd(xmm5, xmm0, xmm1, xmm2).unwrap();
            // vpcmpeqq/vpcmpgtq → opmask (EVEX), then materialize via a masked move.
            a.vpcmpeqq(k1, zmm0, zmm1).unwrap();
            a.vpcmpgtq(k2, zmm0, zmm1).unwrap();
            a.kmovq(rax, k1).unwrap();
            a.kmovq(rcx, k2).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0x0000_0000_FFFF_FFFF_8000_0000_1111_1111;
            c.xmm[1] = 0x0000_0000_FFFF_FFFF_7FFF_FFFF_1111_1111;
            c.xmm[2] = 0x80808080_00000000_FF00FF00_7F7F7F7F;
            c.ymm_hi[0] = 0xDEAD_BEEF_0000_0001_1234_5678_9ABC_DEF0;
            c.ymm_hi[1] = 0xDEAD_BEEF_0000_0000_1234_5678_9ABC_DEF1;
            c.zmm_hi[0] = [0x1, 0x2];
            c.zmm_hi[1] = [0x1, 0x3];
        },
        &[],
    );
}

/// task-215 (TLS): `vextracti32x4 [mem],zmm,imm` / `vextracti64x4 [mem],zmm,imm` —
/// EVEX lane extract to a memory destination.
#[test]
fn extract_lane_mem_dst_match_interp() {
    jit_eq_interp_features(
        GuestCpuFeatures::v4(),
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vextracti32x4(xmmword_ptr(rax), zmm0, 1u32).unwrap();
            a.vextracti32x4(xmmword_ptr(rax + 16), zmm0, 3u32).unwrap();
            a.vextracti64x4(ymmword_ptr(rax + 32), zmm0, 1u32).unwrap();
            a.vmovdqu64(zmm1, zmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[0] = 0x1111_1111_1111_1111_2222_2222_2222_2222;
            c.ymm_hi[0] = 0x3333_3333_3333_3333_4444_4444_4444_4444;
            c.zmm_hi[0] = [
                0x5555_5555_5555_5555_6666_6666_6666_6666,
                0x7777_7777_7777_7777_8888_8888_8888_8888,
            ];
        },
        &[],
    );
}

/// task-219: a boundary-straddling `vextracti64x4 [mem],zmm,imm` (two 128-bit lanes)
/// whose destination's first lane is mapped but second lane is out of the guest buffer
/// must fault IDENTICALLY on interp and JIT — same fault address AND nothing committed.
///
/// The pre-fix interp stored lane-by-lane, so it committed lane 0 then reported the fault
/// at `base + 16`; the JIT does one up-front `checked_addr(base, 32, ..)` that reports the
/// fault at `base` with nothing written. This pins the two together at the atomic-store
/// model (fault at the base, no partial commit).
///
/// A tight, non-page-rounded flat buffer puts the second lane past `memsize` so the JIT's
/// `checked_addr` bounds check faults it (the interp's `region_at` probe faults the same
/// lane), rather than an in-span-but-unmapped hole that a Vec-backed JIT can't guard
/// (decision-3). Runs both backends manually because the `VectorInput` harness always
/// page-rounds the buffer.
#[test]
fn extract_lane_mem_dst_straddle_fault_match_interp() {
    const DST: u64 = 0x8000;
    // Buffer top is exactly the end of lane 0: `[DST, DST+16)` is the last mapped byte,
    // so lane 1 at `[DST+16, DST+32)` runs off the end of the guest buffer.
    const TOP: u64 = DST + 16;

    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(rax, DST).unwrap();
    asm.vextracti64x4(ymmword_ptr(rax), zmm0, 1u32).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    // Distinct low/high halves of the source lane so a partial commit would be visible.
    const LANE0: u128 = 0xAAAA_AAAA_AAAA_AAAA_BBBB_BBBB_BBBB_BBBB;

    let run = |backend: Box<dyn x86jit_core::Backend>| -> (Exit, u64) {
        let mut vm = Vm::with_backend(VmConfig::flat(TOP), backend);
        vm.set_guest_cpu_features(GuestCpuFeatures::v4());
        vm.map(CODE, 0x1000, Prot::RWX, RegionKind::Ram).unwrap();
        vm.map(DST, 16, Prot::RWX, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        // Sentinel in lane 0's slot: a partial store would overwrite it.
        vm.write_bytes(
            DST,
            &0xDEAD_BEEF_DEAD_BEEF_DEAD_BEEF_DEAD_BEEFu128.to_le_bytes(),
        )
        .unwrap();

        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        // zmm0 lane 1 (idx 1) is what `vextracti64x4 ..,1` extracts — the two 128-bit
        // chunks above the low 256 bits, i.e. zmm_hi[0][0] and zmm_hi[0][1].
        cpu.set_zmm_hi(0, 0, LANE0);
        cpu.set_zmm_hi(0, 1, 0xCCCC_CCCC_CCCC_CCCC_DDDD_DDDD_DDDD_DDDD);

        let exit = cpu.run(&vm, Some(16));
        let committed = vm.mem.read(DST, 8).unwrap();
        (exit, committed)
    };

    let (interp_exit, interp_mem) = run(Box::new(InterpreterBackend));
    let (jit_exit, jit_mem) = run(Box::new(JitBackend::new()));

    // Both must fault at the base `DST` with a Write access — no partial commit.
    match interp_exit {
        Exit::UnmappedMemory { addr, access } => {
            assert_eq!(addr, DST, "interp must fault at the store base");
            assert_eq!(access, x86jit_core::AccessKind::Write);
        }
        other => panic!("interp expected UnmappedMemory, got {other:?}"),
    }
    assert_eq!(
        format!("{interp_exit:?}"),
        format!("{jit_exit:?}"),
        "interp and JIT must report the same fault"
    );
    // Lane 0's sentinel must be untouched on both backends (atomic store).
    assert_eq!(
        interp_mem, 0xDEAD_BEEF_DEAD_BEEF,
        "interp committed a partial store"
    );
    assert_eq!(
        jit_mem, 0xDEAD_BEEF_DEAD_BEEF,
        "JIT committed a partial store"
    );
}

/// task-225: `pop [mem]` is restartable — if the destination store faults, RSP must be
/// left un-advanced (fault-before-commit, matching hardware). Pre-fix the lifter wrote
/// RSP back before emitting the destination store, so an `UnmappedMemory{Write}` on the
/// store left RSP already +8. Both backends must now fault at the store with RSP equal
/// to its pre-pop value, and report the identical exit.
#[test]
fn pop_mem_dst_fault_leaves_rsp_unchanged_match_interp() {
    const STACK_TOP: u64 = 0x8000;
    const STACK: u64 = STACK_TOP - 0x100; // mapped stack region below the top
    const RSP0: u64 = STACK_TOP - 8; // pre-pop RSP: one 8-byte slot from the top
                                     // Destination for `pop [rax]`: past the flat buffer, so the store faults.
    const DST: u64 = 0x9_0000;

    let mut asm = CodeAssembler::new(64).unwrap();
    asm.mov(rax, DST).unwrap();
    asm.pop(qword_ptr(rax)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let run = |backend: Box<dyn x86jit_core::Backend>| -> (Exit, u64) {
        let mut vm = Vm::with_backend(VmConfig::flat(STACK_TOP), backend);
        vm.map(CODE, 0x1000, Prot::RWX, RegionKind::Ram).unwrap();
        vm.map(STACK, 0x100, Prot::RWX, RegionKind::Ram).unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        // The value the pop reads off the stack (irrelevant to the fault, just present).
        vm.write_bytes(RSP0, &0x1122_3344_5566_7788u64.to_le_bytes())
            .unwrap();

        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, CODE);
        cpu.set_reg(Reg::Rsp, RSP0);
        let exit = cpu.run(&vm, Some(16));
        (exit, cpu.reg(Reg::Rsp))
    };

    let (interp_exit, interp_rsp) = run(Box::new(InterpreterBackend));
    let (jit_exit, jit_rsp) = run(Box::new(JitBackend::new()));

    // Both must fault at the store destination with a Write access.
    match interp_exit {
        Exit::UnmappedMemory { addr, access } => {
            assert_eq!(addr, DST, "interp must fault at the store destination");
            assert_eq!(access, x86jit_core::AccessKind::Write);
        }
        other => panic!("interp expected UnmappedMemory{{Write}}, got {other:?}"),
    }
    assert_eq!(
        format!("{interp_exit:?}"),
        format!("{jit_exit:?}"),
        "interp and JIT must report the same fault"
    );
    // RSP must NOT have advanced on either backend — the pop is restartable.
    assert_eq!(
        interp_rsp, RSP0,
        "interp advanced RSP past a faulting pop store"
    );
    assert_eq!(jit_rsp, RSP0, "JIT advanced RSP past a faulting pop store");
}

/// task-190: SSE2 packed saturating add/sub (byte + word). Values chosen to hit both
/// the signed clamps (INT_MIN/INT_MAX per lane) and the unsigned clamps (0 / 2^n-1).
#[test]
fn sse2_sat_addsub_match_interp() {
    jit_eq_interp(
        |a| {
            a.paddsb(xmm2, xmm3).unwrap();
            a.paddsw(xmm4, xmm5).unwrap();
            a.paddusb(xmm6, xmm7).unwrap();
            a.paddusw(xmm0, xmm1).unwrap();
            a.psubsb(xmm3, xmm2).unwrap();
            a.psubsw(xmm5, xmm4).unwrap();
            a.psubusb(xmm7, xmm6).unwrap();
            a.psubusw(xmm1, xmm0).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            // Byte lanes: 0x7f+0x7f (signed sat +127), 0x80+0x80 (signed sat -128),
            // 0xff+0x02 (unsigned sat 255), 0x01-0x02 (unsigned sat 0), plus a mix.
            c.xmm[2] = 0x7f7f_8080_0102_03fc_8000_7f01_fefe_0180;
            c.xmm[3] = 0x7f01_8080_ff02_04fd_0180_0180_0203_ff7f;
            // Word lanes: signed/unsigned boundary mix.
            c.xmm[4] = 0x7fff_8000_0001_ffff_1234_8000_7fff_0000;
            c.xmm[5] = 0x7fff_8000_ffff_0001_edcc_8000_0001_8000;
            c.xmm[6] = 0xff00_0102_8080_7f7f_ffff_0000_1234_abcd;
            c.xmm[7] = 0x0102_ff00_8080_0101_0001_ffff_dcba_5432;
            c.xmm[0] = 0xffff_0001_8000_7fff_1000_0fff_ffff_0000;
            c.xmm[1] = 0x0002_ffff_8000_0001_0fff_1000_0001_ffff;
        },
        &[],
    );
}

/// task-190: packed unsigned rounding average `pavgb`/`pavgw` — verifies the `+1`
/// rounding (e.g. (0+1+1)>>1 == 1, (0xff+0xff+1)>>1 == 0xff, no overflow at max).
#[test]
fn sse2_pavg_match_interp() {
    jit_eq_interp(
        |a| {
            a.pavgb(xmm2, xmm3).unwrap();
            a.pavgw(xmm4, xmm5).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            c.xmm[2] = 0x0000_00ff_ff01_0203_8081_fefd_1122_3344;
            c.xmm[3] = 0x0001_00ff_ff02_0304_8182_0102_1123_3345;
            c.xmm[4] = 0x0000_ffff_0001_8000_1234_fffe_7fff_0001;
            c.xmm[5] = 0x0001_ffff_0002_8001_1235_0001_8000_0002;
        },
        &[],
    );
}

/// task-190: signed packs `packsswb` (word->byte) and `packssdw` (dword->word).
/// Pins the x86 lane order (dst's lanes fill the low half, src's the high) and the
/// signed saturation at each narrower lane's INT_MIN/INT_MAX.
#[test]
fn sse2_signed_packs_match_interp() {
    jit_eq_interp(
        |a| {
            a.packsswb(xmm2, xmm3).unwrap();
            a.packssdw(xmm4, xmm5).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            // words: +0x7fff -> +127, -0x8000 -> -128, small values pass through.
            c.xmm[2] = 0x7fff_8000_0080_ff80_0001_ffff_007f_ff81;
            c.xmm[3] = 0x00ff_ff01_1234_edcc_007f_ff80_0100_fe00;
            // dwords: +0x7fffffff -> +0x7fff, -0x80000000 -> -0x8000, pass-through.
            c.xmm[4] = 0x7fff_ffff_8000_0000_0000_7fff_ffff_8000;
            c.xmm[5] = 0x0001_2345_ffff_edcb_0000_0001_ffff_ffff;
        },
        &[],
    );
}

/// task-190: `pmaddwd` — pairwise signed 16x16 multiply, adjacent products summed into
/// signed dwords. Includes the sole i32-overflowing case (0x8000*0x8000 + 0x8000*0x8000)
/// which must wrap two's-complement to match hardware.
#[test]
fn sse2_pmaddwd_match_interp() {
    jit_eq_interp(
        |a| {
            a.pmaddwd(xmm2, xmm3).unwrap();
            a.hlt().unwrap();
        },
        |c| {
            // dword0 lanes: 0x8000*0x8000 + 0x8000*0x8000 = 0x8000_0000 (overflow/wrap).
            c.xmm[2] = 0x0001_0002_ffff_7fff_8000_8000_8000_8000;
            c.xmm[3] = 0x0003_0004_0002_7fff_8000_8000_8000_8000;
        },
        &[],
    );
}

/// Packed float↔int converts (task-239): JIT == interpreter bit-for-bit, including the
/// saturating out-of-range / NaN edges (both legs use the same `as`-cast convention, so
/// they agree even where real hardware would emit the deferred integer-indefinite value —
/// hence this is a JIT-vs-interp check, not a unicorn one). Covers NaN, ±inf, huge
/// magnitudes, negatives, and the round-vs-truncate split.
#[test]
fn cvt_packed_match_interp() {
    jit_eq_interp(
        |a| {
            // f32×4 [NaN, +inf, 3.0e20, -3.0e20] — all out of i32 range / invalid.
            a.mov(rax, 0x7F80_0000_7FC0_0000u64).unwrap(); // NaN, +inf
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xE082_3179_6082_3179u64).unwrap(); // 3.0e20, -3.0e20 (approx bits)
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm0, xmmword_ptr(SCRATCH)).unwrap();
            a.cvtps2dq(xmm1, xmm0).unwrap();
            a.cvttps2dq(xmm2, xmm0).unwrap();

            // f64×2 [NaN, 1.0e300] — both out of i32 range / invalid.
            a.mov(rax, 0x7FF8_0000_0000_0000u64).unwrap(); // NaN
            a.mov(qword_ptr(SCRATCH + 16), rax).unwrap();
            a.mov(rax, 0x7E37_E43C_8800_759Cu64).unwrap(); // 1.0e300
            a.mov(qword_ptr(SCRATCH + 24), rax).unwrap();
            a.movdqu(xmm3, xmmword_ptr(SCRATCH + 16)).unwrap();
            a.cvtpd2dq(xmm4, xmm3).unwrap();
            a.cvttpd2dq(xmm5, xmm3).unwrap();

            // In-range round-vs-truncate + widen/narrow round-trip for good measure.
            a.mov(rax, 0xC020_0000_3FC0_0000u64).unwrap(); // f32 [1.5, -2.5]
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0x0000_0000_0000_0000u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm6, xmmword_ptr(SCRATCH)).unwrap();
            a.cvtps2pd(xmm7, xmm6).unwrap();
            a.cvtpd2ps(xmm8, xmm7).unwrap();
            a.cvtdq2pd(xmm9, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// JIT == interpreter for register-count packed shifts `psll/psrl/psra {w,d,q} xmm, xmm`
/// (task-237 native path). Same coverage as the differential test — logical L/R, arith R,
/// over-shift across word/dword/qword — asserting the native JIT lowering equals the
/// (helper-backed) interpreter bit-for-bit.
#[test]
fn shift_reg_count_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0x8899_AABB_1122_3344u64).unwrap();
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xF000_0001_0000_0010u64).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm0, xmmword_ptr(SCRATCH)).unwrap();
            a.mov(rax, 3u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.movdqa(xmm2, xmm0).unwrap();
            a.pslld(xmm2, xmm1).unwrap();
            a.movdqa(xmm3, xmm0).unwrap();
            a.psrld(xmm3, xmm1).unwrap();
            a.movdqa(xmm4, xmm0).unwrap();
            a.psrad(xmm4, xmm1).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.psllw(xmm5, xmm1).unwrap();
            a.movdqa(xmm6, xmm0).unwrap();
            a.psrlq(xmm6, xmm1).unwrap();
            a.mov(rax, 40u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.movdqa(xmm7, xmm0).unwrap();
            a.psrld(xmm7, xmm1).unwrap();
            a.movdqa(xmm8, xmm0).unwrap();
            a.psrad(xmm8, xmm1).unwrap();
            a.movdqa(xmm9, xmm0).unwrap();
            a.psllw(xmm9, xmm1).unwrap();
            a.mov(rax, 100u64).unwrap();
            a.movq(xmm1, rax).unwrap();
            a.movdqa(xmm10, xmm0).unwrap();
            a.psrlq(xmm10, xmm1).unwrap();
            a.movdqa(xmm11, xmm0).unwrap();
            a.psraw(xmm11, xmm1).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// Upper-bits (bits 255:128) handling for register-count shifts (task-237): the legacy-SSE
/// form preserves the destination's stale YMM upper, the VEX.128 form clears it. Asserts
/// the native JIT lowering matches the interpreter for BOTH (seeded stale upper).
#[test]
fn shift_reg_upper_bits_match_interp() {
    jit_eq_interp(
        |a| {
            a.pslld(xmm0, xmm1).unwrap(); // SSE: preserve ymm_hi[0]
            a.vpslld(xmm3, xmm0, xmm1).unwrap(); // VEX.128: zero ymm_hi[3]
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0x0000_0004_0000_0003_0000_0002_0000_0001;
            s.xmm[1] = 2;
            s.ymm_hi[0] = 0x0000_DEAD_BEEF_CAFE; // SSE dst upper — must survive
            s.ymm_hi[3] = 0x0000_1234_5678_9ABC; // VEX dst upper — must clear
        },
        &[],
    );
}

/// MOVMSKPS / MOVMSKPD (task-240): JIT == interpreter for the sign-mask extraction,
/// including the Doom `movmskpd %xmm0,%esi` encoding and mixed sign patterns.
#[test]
fn movmsk_ps_pd_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, 0xBFF0_0000_0000_0000u64).unwrap(); // f64 [-1,-1]
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm0, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskpd(esi, xmm0).unwrap();

            a.mov(rax, 0x4000_0000_0000_0000u64).unwrap(); // +2.0
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0xC008_0000_0000_0000u64).unwrap(); // -3.0
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm1, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskpd(edi, xmm1).unwrap();

            a.mov(rax, 0x4000_0000_BF80_0000u64).unwrap(); // f32 -1,+2
            a.mov(qword_ptr(SCRATCH), rax).unwrap();
            a.mov(rax, 0x4080_0000_C040_0000u64).unwrap(); // -3,+4
            a.mov(qword_ptr(SCRATCH + 8), rax).unwrap();
            a.movdqu(xmm2, xmmword_ptr(SCRATCH)).unwrap();
            a.movmskps(eax, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// task-243: integer unpack/pack with a 128-bit memory source 2 — the JIT `_m` emit paths
/// (`emit_v_unpack_low_m` inline shuffle, `emit_v_pack_wide_m` via the `vpack_mem` helper)
/// must match the interpreter. Covers the legacy in-place forms and the VEX.128 forms
/// (with upper-zeroing) including the Mono blocker `vpunpckldq [mem], xmm0, xmm0`.
#[test]
fn unpack_pack_mem_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm2).unwrap(); // stage src2 in memory
            a.punpckldq(xmm0, xmmword_ptr(rax)).unwrap();
            a.punpckhbw(xmm1, xmmword_ptr(rax)).unwrap();
            a.packssdw(xmm3, xmmword_ptr(rax)).unwrap();
            a.vpunpckldq(xmm4, xmm4, xmmword_ptr(rax)).unwrap(); // blocker shape
            a.vpunpckhwd(xmm5, xmm6, xmmword_ptr(rax)).unwrap(); // non-destructive
            a.vpackssdw(xmm7, xmm6, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
            s.xmm[1] = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
            s.xmm[2] = 0x1F1E_1D1C_1B1A_1918_1716_1514_1312_1110; // the memory operand
            s.xmm[3] = 0x0001_0002_FFFF_FFFE_7FFF_FFFF_8000_0000;
            s.xmm[4] = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
            s.xmm[6] = 0x0001_0002_FFFF_FFFE_7FFF_FFFF_8000_0000;
        },
        &[],
    );
}

/// task-244: SSE3 lane-combining packed float `h{add,sub}p`/`addsubp` — the JIT paths
/// (`emit_v_h_float` via the `vhfloat` helper, `emit_v_h_float_m` via `vhfloat_mem`) must
/// match the interpreter, register and 128-bit memory source, both precisions and the
/// VEX.128 forms (with upper-zeroing) including the blocker `vhaddpd xmm0, xmm0, xmm0`.
#[test]
fn hadd_addsub_mem_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap(); // f64 src2 in memory
            a.movdqa(xmm4, xmm0).unwrap();
            a.haddpd(xmm4, xmmword_ptr(rax)).unwrap();
            a.vaddsubpd(xmm5, xmm0, xmmword_ptr(rax)).unwrap(); // VEX mem form

            a.movdqu(xmmword_ptr(rax), xmm3).unwrap(); // f32 src2 in memory
            a.movdqa(xmm6, xmm2).unwrap();
            a.hsubps(xmm6, xmmword_ptr(rax)).unwrap();

            a.vhaddpd(xmm7, xmm0, xmm0).unwrap(); // the blocker (register)
            a.vaddsubps(xmm8, xmm3, xmm2).unwrap(); // VEX register form
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0x4004_0000_0000_0000_3FF8_0000_0000_0000; // [1.5, 2.5]
            s.xmm[1] = 0x4034_0000_0000_0000_4024_0000_0000_0000; // [10.0, 20.0]
            s.xmm[2] = 0x4080_0000_4040_0000_4000_0000_3F80_0000; // [1,2,3,4]
            s.xmm[3] = 0x4220_0000_41F0_0000_41A0_0000_4120_0000; // [10,20,30,40]
        },
        &[],
    );
}

/// task-246: non-temporal moves lower like the aligned movdqa/movaps path — the JIT must
/// match the interpreter for the VEX store (`vmovntdq [mem], xmm`, the blocker) and load
/// (`vmovntdqa`, with upper-zeroing) and the legacy `movntdqa` load.
#[test]
fn movnt_moves_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovntdq(xmmword_ptr(rax), xmm1).unwrap(); // the blocker
            a.vmovntps(xmmword_ptr(rax + 16), xmm2).unwrap();
            a.vmovntdqa(xmm4, xmmword_ptr(rax)).unwrap(); // VEX load (upper-zero)
            a.movntdqa(xmm5, xmmword_ptr(rax + 16)).unwrap(); // legacy load
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[1] = 0x0F0E_0D0C_0B0A_0908_0706_0504_0302_0100;
            s.xmm[2] = 0xF0F1_F2F3_F4F5_F6F7_F8F9_FAFB_FCFD_FEFF;
        },
        &[],
    );
}

/// task-247: SSSE3 packed-integer horizontal `ph{add,sub}{w,d,sw}` — the JIT paths
/// (`emit_v_h_int` via the `vhint` helper, `emit_v_h_int_m` via `vhint_mem`) must match
/// the interpreter, register and 128-bit memory source, including the saturating `sw`
/// variants and the VEX.128 forms (upper-zeroing) with the blocker `vphaddd xmm0,xmm0,xmm0`.
#[test]
fn phadd_phsub_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap(); // word src2 in memory
            a.movdqa(xmm4, xmm0).unwrap();
            a.phaddsw(xmm4, xmmword_ptr(rax)).unwrap(); // legacy mem, saturating
            a.vphaddw(xmm5, xmm0, xmmword_ptr(rax)).unwrap(); // VEX mem form

            a.movdqu(xmmword_ptr(rax), xmm3).unwrap(); // dword src2 in memory
            a.movdqa(xmm6, xmm2).unwrap();
            a.phsubd(xmm6, xmmword_ptr(rax)).unwrap();

            a.vphaddd(xmm7, xmm0, xmm0).unwrap(); // the blocker (register)
            a.vphsubsw(xmm8, xmm1, xmm0).unwrap(); // VEX register, saturating
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0x4000_4000_FFFF_0001_8000_8000_7FFF_7FFF;
            s.xmm[1] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
            s.xmm[2] = 0x7FFF_FFFF_7FFF_FFFF_0000_0002_0000_0003;
            s.xmm[3] = 0x8000_0000_8000_0000_FFFF_FFFF_0000_0005;
        },
        &[],
    );
}

/// One VEX.128 form from each family lifted in task-242/243/244/246/247, with the
/// destination's upper 128 bits (`ymm_hi`) pre-dirtied so the JIT's `VZeroUpper` is
/// actively validated: if a JIT emit path forgot to clear bits[255:128], `jit != interp`
/// (the interpreter zeroes them). The op-specific `*_zeroes_ymm_upper` differential tests
/// run the interpreter only; this exercises the JIT side of the same invariant.
#[test]
fn vex128_new_ops_zero_ymm_upper_jit_eq_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.vroundpd(xmm4, xmm0, 1u32).unwrap(); // task-242 (floor)
            a.vpunpckldq(xmm5, xmm0, xmmword_ptr(rax)).unwrap(); // task-243 (mem)
            a.vaddsubpd(xmm6, xmm0, xmm1).unwrap(); // task-244 (reg)
            a.vmovntdqa(xmm7, xmmword_ptr(rax)).unwrap(); // task-246 (load)
            a.vphaddw(xmm8, xmm0, xmm1).unwrap(); // task-247 (reg)
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0x4004_0000_0000_0000_3FF8_0000_0000_0000; // [1.5, 2.5]
            s.xmm[1] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
            // Dirty every VEX destination's upper 128 bits so the zeroing is observable.
            for r in 4..=8 {
                s.ymm_hi[r] = u128::MAX;
            }
        },
        &[],
    );
}

/// task-253: SSE3 duplicating moves + their VEX.128 forms — JIT must match interp across
/// register and memory sources (incl. movddup's m64), with the VEX dsts' ymm_hi dirtied so
/// the upper-zeroing is observable.
#[test]
fn movdup_family_match_interp() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm0).unwrap();
            a.movddup(xmm1, xmm0).unwrap();
            a.movsldup(xmm2, xmm0).unwrap();
            a.movshdup(xmm3, xmm0).unwrap();
            a.movddup(xmm4, qword_ptr(rax)).unwrap(); // m64
            a.vmovddup(xmm5, xmm0).unwrap(); // VEX reg
            a.vmovsldup(xmm6, xmmword_ptr(rax)).unwrap(); // VEX m128
            a.vmovshdup(xmm7, xmm0).unwrap();
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0x3333_3333_2222_2222_1111_1111_0000_0000;
            for r in [5usize, 6, 7] {
                s.ymm_hi[r] = u128::MAX; // VEX dsts must be zeroed
            }
        },
        &[],
    );
}

/// task-252: VEX.128 `vmovlhps`/`vmovhlps` — JIT must match interp, including the wild
/// `dst == src2` alias and VEX upper-zeroing (dirty ymm_hi so the zeroing is observable).
#[test]
fn vmovlhps_vmovhlps_match_interp() {
    jit_eq_interp(
        |a| {
            a.vmovlhps(xmm5, xmm0, xmm1).unwrap();
            a.vmovhlps(xmm6, xmm0, xmm1).unwrap();
            a.vmovlhps(xmm0, xmm1, xmm0).unwrap(); // wild shape: dst aliases src2
            a.hlt().unwrap();
        },
        |s| {
            s.xmm[0] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
            s.xmm[1] = 0xAAAA_AAAA_BBBB_BBBB_CCCC_CCCC_DDDD_DDDD;
            for r in [0usize, 5, 6] {
                s.ymm_hi[r] = u128::MAX;
            }
        },
        &[],
    );
}

// ---- Register-survival regression (task-241) ----
//
// The task-242..249 SIMD lifts (round, unpack/pack, horizontal float/int, addsub,
// psadbw, non-temporal moves) were only covered by hand-written jit.rs cases that
// set a handful of registers, so a lowering that clobbered an *unrelated* live
// register (the shape task-241 suspected) would leave both the interp and the JIT
// at the default 0 for that register and slip through `compare`. These tests
// pre-load a FULL sentinel register file — every GPR (bar the two the snippet needs
// for addressing/stack), all 16 XMMs, and every ymm_hi — before each op, so any
// register the JIT writes but the interpreter doesn't shows up as a divergence.
// All pass: the JIT preserves every unrelated register across these ops.

/// Pre-load a full sentinel register file (GPRs except RAX/RSP which the snippet
/// needs, all 16 XMMs, all ymm_hi), so any register a JIT lowering clobbers but
/// the interpreter doesn't shows up in `compare` as a divergence.
fn survival_init(s: &mut CpuSnapshot) {
    // Distinct GPR sentinels. Leave RAX (idx 0) for the scratch base and RSP (idx 4)
    // for the stack; the snippet sets RAX itself.
    for i in 0..16 {
        if i == 0 || i == 4 {
            continue;
        }
        s.gpr[i] = 0xC0DE_0000_0000_0000 | (i as u64 * 0x0101_0101_0101);
    }
    // Distinct XMM sentinels + dirty ymm upper.
    for i in 0..16 {
        s.xmm[i] = (0xA5A5_0000_0000_0000_0000_0000_0000_0000u128) | (i as u128);
        s.ymm_hi[i] = 0x5A5A_0000_0000_0000_0000_0000_0000_0000u128 | (i as u128);
    }
    // Put some meaningful-ish packed data in the operand XMMs.
    s.xmm[0] = 0x4004_0000_0000_0000_3FF8_0000_0000_0000;
    s.xmm[1] = 0x1111_2222_3333_4444_5555_6666_7777_8888;
    s.xmm[2] = 0x7FFF_FFFF_7FFF_FFFF_0000_0002_0000_0003;
    s.xmm[3] = 0x8000_0000_8000_0000_FFFF_FFFF_0000_0005;
}

#[test]
fn survival_vround() {
    jit_eq_interp(
        |a| {
            a.vroundpd(xmm5, xmm0, 1u32).unwrap();
            a.vroundss(xmm6, xmm2, xmm3, 0u32).unwrap();
            a.vroundsd(xmm7, xmm2, xmm3, 2u32).unwrap();
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}

#[test]
fn survival_unpack_pack_mem() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.punpckldq(xmm5, xmmword_ptr(rax)).unwrap();
            a.packssdw(xmm6, xmmword_ptr(rax)).unwrap();
            a.vpunpckldq(xmm7, xmm7, xmmword_ptr(rax)).unwrap();
            a.vpackssdw(xmm8, xmm2, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}

#[test]
fn survival_hfloat() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.haddpd(xmm5, xmmword_ptr(rax)).unwrap();
            a.vaddsubpd(xmm6, xmm0, xmm1).unwrap();
            a.vhaddpd(xmm7, xmm0, xmm0).unwrap();
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}

#[test]
fn survival_hint() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.phaddw(xmm5, xmmword_ptr(rax)).unwrap();
            a.vphaddd(xmm6, xmm0, xmm0).unwrap();
            a.vphsubsw(xmm7, xmm1, xmm0).unwrap();
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}

#[test]
fn survival_psadbw() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.psadbw(xmm5, xmmword_ptr(rax)).unwrap();
            a.vpsadbw(xmm6, xmm0, xmm1).unwrap();
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}

#[test]
fn survival_cmpss() {
    // `cmpss` register- and memory-operand forms with a full sentinel register
    // file: a scalar compare must write only the dest's low dword and preserve
    // every unrelated register. Covers an ordered (LT=1) and an unordered/negated
    // (NEQ=4) predicate against both a register and a memory comparand.
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(rax), xmm1).unwrap();
            a.movdqa(xmm5, xmm0).unwrap();
            a.cmpss(xmm5, xmm2, 1u32).unwrap(); // reg, LT
            a.movdqa(xmm6, xmm0).unwrap();
            a.cmpss(xmm6, xmm3, 4u32).unwrap(); // reg, NEQ
            a.movdqa(xmm7, xmm0).unwrap();
            a.cmpss(xmm7, dword_ptr(rax), 1u32).unwrap(); // mem, LT
            a.movdqa(xmm8, xmm0).unwrap();
            a.cmpss(xmm8, dword_ptr(rax), 4u32).unwrap(); // mem, NEQ
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}

#[test]
fn survival_addr_reg_variants() {
    // Memory-source forms with the address in a NON-RAX GPR (R11 here), and high
    // XMM/GPR indices, to catch a lowering that trashes the address-holding GPR or a
    // REX-encoded register only under specific operand combos.
    jit_eq_interp(
        |a| {
            a.mov(r11, SCRATCH).unwrap();
            a.movdqu(xmmword_ptr(r11), xmm9).unwrap();
            a.vpunpckldq(xmm10, xmm11, xmmword_ptr(r11)).unwrap();
            a.vpackssdw(xmm12, xmm13, xmmword_ptr(r11)).unwrap();
            a.haddpd(xmm14, xmmword_ptr(r11)).unwrap();
            a.vaddsubpd(xmm15, xmm9, xmmword_ptr(r11)).unwrap();
            a.phaddw(xmm8, xmmword_ptr(r11)).unwrap();
            a.psadbw(xmm7, xmmword_ptr(r11)).unwrap();
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}

#[test]
fn survival_movnt() {
    jit_eq_interp(
        |a| {
            a.mov(rax, SCRATCH).unwrap();
            a.vmovntdq(xmmword_ptr(rax), xmm1).unwrap();
            a.vmovntdqa(xmm5, xmmword_ptr(rax)).unwrap();
            a.movntdqa(xmm6, xmmword_ptr(rax)).unwrap();
            a.hlt().unwrap();
        },
        survival_init,
        &[],
    );
}
