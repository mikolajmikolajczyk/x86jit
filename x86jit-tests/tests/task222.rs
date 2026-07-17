//! task-222 acceptance: three x86-semantics fixes, each pinned interp==JIT (and,
//! where an oracle is practical, against concrete hardware-defined values).
//!
//!  * Bug 1 — the AMD64 `syscall` instruction latches RCX <- next-RIP and R11 <-
//!    RFLAGS; the i386 `int 0x80` gate must NOT touch RCX/R11.
//!  * Bug 2 — `fnstsw m16` stores the 16-bit status word to memory (not just AX).
//!  * Bug 3 — `rep movs` honours the 67h 32-bit address size (ESI/EDI/ECX + wrap)
//!    and an FS/GS segment override on the DS-relative source pointer.
//!
//! Self-contained differential plumbing (mirrors `addr32.rs`): assemble a single
//! `hlt`-terminated block, run it on x86jit's interpreter and JIT, and compare the
//! resulting `CpuState`. Long-mode cases decode `Long64`; the 67h string case uses a
//! 64-bit block with a `67h`-prefixed `rep movs`.

use iced_x86::code_asm::*;

use x86jit_core::jit_abi::run_compiled;
use x86jit_core::lift::{lift_block, CpuMode, FetchAddr};
use x86jit_core::state::CpuState;
use x86jit_core::{
    CachedBlock, Exit, InterpreterBackend, Prot, RegionKind, StepResult, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const CODE: u64 = 0x1000;
const CODE_LEN: usize = 0x1000;
const SCRATCH: u64 = 0x8000;
const SCRATCH_LEN: usize = 0x2000;

/// Initial guest state a case sets up before the run.
#[derive(Clone, Default)]
struct Init {
    gpr: [u64; 16],
    fs_base: u64,
    df: bool,
    /// (offset-from-SCRATCH, bytes) writes into the scratch page before the run.
    data: Vec<(u64, Vec<u8>)>,
}

/// The observable end state of one engine: full GPRs, flags-as-RFLAGS, and the
/// scratch page (so memory stores are visible).
struct Outcome {
    cpu: CpuState,
    scratch: Vec<u8>,
    exit: Exit,
}

/// Run one `hlt`-terminated block on x86jit under `mode`, once per backend.
fn run(code: &[u8], init: &Init, mode: CpuMode, jit: bool) -> Outcome {
    let backend: Box<dyn x86jit_core::Backend> = if jit {
        Box::new(JitBackend::new())
    } else {
        Box::new(InterpreterBackend)
    };
    let mut vm = Vm::with_backend(VmConfig::flat(0x1_0000), backend);
    vm.map(CODE, CODE_LEN, Prot::RX, RegionKind::Ram).unwrap();
    vm.map(SCRATCH, SCRATCH_LEN, Prot::RW, RegionKind::Ram)
        .unwrap();
    vm.write_bytes(CODE, code).unwrap();
    for (off, bytes) in &init.data {
        vm.write_bytes(SCRATCH + off, bytes).unwrap();
    }

    let mut vcpu = vm.new_vcpu();
    vcpu.cpu.gpr = init.gpr;
    vcpu.cpu.fs_base = init.fs_base;
    vcpu.cpu.flags.df = init.df;
    vcpu.cpu.rip = CODE;

    let ir = lift_block(&vm.mem, FetchAddr::flat(CODE), mode).expect("lift block");
    let result = if jit {
        let entry = match vm.backend.materialize(
            &ir,
            vm.consistency,
            vm.mem.trap_window(),
            vm.mem.guest_base(),
        ) {
            CachedBlock::Compiled { entry, .. } => entry,
            _ => panic!("JIT backend must compile the block"),
        };
        // SAFETY: freshly compiled block for `vm`'s memory, run once.
        unsafe { run_compiled(entry, &mut vcpu.cpu, &vm.mem, mode) }
    } else {
        let mut scratch = Vec::new();
        x86jit_core::interp::interpret_block(&ir, &mut vcpu.cpu, &vm.mem, &mut scratch)
    };
    let exit = match result {
        StepResult::Exit(e) => e,
        StepResult::Continue => panic!("block did not terminate at a block-ending op"),
    };

    let mut scratch = vec![0u8; SCRATCH_LEN];
    vm.read_bytes(SCRATCH, &mut scratch).unwrap();
    Outcome {
        cpu: vcpu.cpu,
        scratch,
        exit,
    }
}

/// Run both engines and assert bit-for-bit agreement on the observable state.
fn interp_jit_agree(code: &[u8], init: &Init, mode: CpuMode) -> Outcome {
    let i = run(code, init, mode, false);
    let j = run(code, init, mode, true);
    assert_eq!(i.cpu.gpr, j.cpu.gpr, "interp/jit GPR mismatch");
    assert_eq!(i.cpu.flags, j.cpu.flags, "interp/jit flags mismatch");
    assert_eq!(i.scratch, j.scratch, "interp/jit scratch memory mismatch");
    assert_eq!(
        format!("{:?}", i.exit),
        format!("{:?}", j.exit),
        "interp/jit exit mismatch"
    );
    i
}

/// Bug 1: the AMD64 `syscall` instruction latches RCX <- next-instruction RIP and
/// R11 <- RFLAGS. interp==JIT, and both match the hardware-defined values.
#[test]
fn syscall_sets_rcx_and_r11() {
    // A single `syscall` (0f 05), then `hlt` (never reached — syscall ends the block).
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.syscall().unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();
    let syscall_len = code.len() as u64 - 1; // block ends after `syscall`, before `hlt`

    // RCX/R11 must be OVERWRITTEN (with return-RIP / RFLAGS); DF (bit 10) must appear
    // in the latched RFLAGS. We can't run an ALU op in this single-syscall block, so
    // seed the flag field directly.
    let mut gpr = [0u64; 16];
    gpr[1] = 0xDEAD_BEEF;
    gpr[11] = 0x1234;
    let init = Init {
        gpr,
        df: true,
        ..Default::default()
    };

    let out = interp_jit_agree(&code, &init, CpuMode::Long64);
    assert!(matches!(out.exit, Exit::Syscall), "syscall must exit-out");

    let ret_rip = CODE + syscall_len;
    assert_eq!(out.cpu.gpr[1], ret_rip, "RCX = next-instruction RIP");
    // RFLAGS: reserved bit 1 always set, plus DF (bit 10) from our init.
    let expected_r11 = (1u64 << 1) | (1u64 << 10);
    assert_eq!(
        out.cpu.gpr[11], expected_r11,
        "R11 = RFLAGS (reserved + DF)"
    );
}

/// Bug 1 (regression guard): the i386 `int 0x80` gate must NOT clobber RCX/R11 — its
/// ABI passes the syscall args in EBX/ECX/… A naive unconditional latch corrupted a
/// 32-bit `write(2)`'s buffer pointer in ECX. Decoded `Compat32`.
#[test]
fn int80_does_not_touch_rcx_r11() {
    // `int 0x80` = CD 80, then a padding byte so the block has somewhere to sit.
    let code = [0xCD, 0x80, 0xf4];

    let mut gpr = [0u64; 16];
    gpr[1] = 0xC0FF_EE00; // ECX — an i386 syscall arg, must survive untouched
    gpr[11] = 0xABCD_1234; // R11 — must survive untouched
    let init = Init {
        gpr,
        df: true,
        ..Default::default()
    };

    let out = interp_jit_agree(&code, &init, CpuMode::Compat32);
    assert!(matches!(out.exit, Exit::Syscall), "int 0x80 must exit-out");
    assert_eq!(
        out.cpu.gpr[1], 0xC0FF_EE00,
        "int 0x80 must NOT overwrite RCX"
    );
    assert_eq!(
        out.cpu.gpr[11], 0xABCD_1234,
        "int 0x80 must NOT overwrite R11"
    );
}

/// Bug 2: `fnstsw m16` stores the 16-bit x87 status word to memory (the memory form,
/// as opposed to `fnstsw ax`). The status word's TOP field (bits 11–13) reflects the
/// FPU stack top. interp==JIT and the stored halfword matches.
#[test]
fn fnstsw_m16_stores_status_word() {
    // `fnstsw [SCRATCH]`. Push one value first so TOP != 0 and the store is non-trivial.
    let mut asm = CodeAssembler::new(64).unwrap();
    asm.fld1().unwrap(); // push 1.0 → TOP decrements to 7
    asm.fnstsw(word_ptr(SCRATCH)).unwrap();
    asm.hlt().unwrap();
    let code = asm.assemble(CODE).unwrap();

    let out = interp_jit_agree(&code, &Init::default(), CpuMode::Long64);
    let sw = u16::from_le_bytes([out.scratch[0], out.scratch[1]]);
    // After one push, TOP = 7 → bits 11..13 = 0b111 = 0x3800.
    assert_eq!(sw, 0x3800, "fnstsw m16 stored status word with TOP=7");
}

/// Bug 3: a 67h-prefixed `rep movsb` uses 32-bit ESI/EDI/ECX and wraps mod 2^32. Copy
/// 4 bytes from SCRATCH+0 to SCRATCH+0x100 with ECX=4 under a 32-bit address size.
/// interp==JIT and the destination bytes match the source.
#[test]
fn rep_movs_addr32() {
    // `67 f3 a4` = addr-size `rep movsb`. hand-encoded (iced won't emit the 67h form
    // for a rep movs directly), then `hlt`.
    let code = [0x67, 0xf3, 0xa4, 0xf4];

    let mut gpr = [0u64; 16];
    gpr[6] = SCRATCH; // ESI (source)
    gpr[7] = SCRATCH + 0x100; // EDI (dest)
    gpr[1] = 4; // ECX (count)
    let init = Init {
        gpr,
        data: vec![(0, vec![0x11, 0x22, 0x33, 0x44])],
        ..Default::default()
    };

    let out = interp_jit_agree(&code, &init, CpuMode::Long64);
    assert_eq!(
        &out.scratch[0x100..0x104],
        &[0x11, 0x22, 0x33, 0x44],
        "rep movsb (67h) copied the 4 source bytes"
    );
    // ECX consumed to 0; ESI/EDI advanced by 4 (32-bit result, upper bits zero).
    assert_eq!(out.cpu.gpr[1], 0, "ECX drained");
    assert_eq!(out.cpu.gpr[6], SCRATCH + 4, "ESI advanced");
    assert_eq!(out.cpu.gpr[7], SCRATCH + 0x104, "EDI advanced");
}

/// Bug 3: an FS segment override redirects the DS-relative source of `movs`. With a
/// live nonzero FS base, `rep movs fs:[rsi]` reads from `fs_base + rsi`, while the
/// ES:[rdi] destination is unaffected. interp==JIT and the copied bytes come from the
/// FS-based source, not the bare RSI.
#[test]
fn rep_movs_fs_override() {
    // `f3 64 a4` = `rep movs es:[rdi], fs:[rsi]` (F3 rep, 64 = FS prefix, A4 = movsb).
    let code = [0xf3, 0x64, 0xa4, 0xf4];

    // fs_base = SCRATCH+0x200; the source bytes live there. RSI = 0 so the bare read
    // (buggy) would hit SCRATCH+0 (a different, zero-filled region).
    let mut gpr = [0u64; 16];
    gpr[6] = 0; // RSI: fs_base + 0 = SCRATCH+0x200
    gpr[7] = SCRATCH + 0x300; // RDI (dest)
    gpr[1] = 3; // RCX
    let init = Init {
        gpr,
        fs_base: SCRATCH + 0x200,
        data: vec![
            (0x200, vec![0xAA, 0xBB, 0xCC]), // FS-based source
            (0, vec![0x00, 0x00, 0x00]),     // bare RSI would (wrongly) read these zeros
        ],
        ..Default::default()
    };

    let out = interp_jit_agree(&code, &init, CpuMode::Long64);
    assert_eq!(
        &out.scratch[0x300..0x303],
        &[0xAA, 0xBB, 0xCC],
        "movs read from fs:[rsi] = fs_base, not bare rsi"
    );
}
