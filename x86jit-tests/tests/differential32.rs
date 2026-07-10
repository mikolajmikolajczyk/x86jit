//! 32-bit (`CpuMode::Compat32`) differential acceptance (task-197.5, MODE-A.5):
//! for each snippet the interpreter running in 32-bit compat mode must equal
//! Unicorn's `UC_MODE_32`. This is the safety net every other MODE-A subtask cites.
//!
//! ```text
//! cargo nextest run -p x86jit-tests --features unicorn -E 'test(/32/)'
//! ```
//!
//! ## Lane structure
//!
//! - **Mode-neutral cases** (arithmetic, logic, mov, inc/dec 0x40–0x4F, shifts,
//!   setcc/cmov, SSE) share the same encodings a 64-bit guest uses; they pass on
//!   pure task-197.1 plumbing and run un-ignored — they are the proof the lane works.
//! - **32-bit-only cases** (address wrap at 4 GiB, 67h 16-bit addressing, EIP wrap,
//!   16-bit/32-bit stack widths) depend on execution *semantics* that land on sibling
//!   branches: task-197.2 (address wrap / 67h) and task-197.3 (EIP wrap / stack
//!   widths). Those aren't on this branch, so the cases are `#[ignore]`d with an
//!   explicit task tag. Integration flips them on after the semantics merge.
//!
//! The 0x40–0x4F `inc`/`dec` short forms — REX prefixes in long mode — decode and
//! execute here on plumbing alone (the lifter already lifts Inc/Dec), so those cases
//! are un-ignored: they prove the mode's decode path is live.

#![cfg(feature = "unicorn")]

use iced_x86::code_asm::*;
use x86jit_core::{CpuMode, GuestCpuFeatures, InterpreterBackend};
use x86jit_tests::compare::compare;
use x86jit_tests::oracle::{run_with_backend_mode, Oracle, VectorInput};
use x86jit_tests::unicorn::UnicornOracle32;
use x86jit_tests::vector::{CpuSnapshot, FlagName, MemChunk, MemKind, RunSpec};

/// Fixed low-memory entry and scratch page — flat 32-bit layout, well under 4 GiB.
const ENTRY: u64 = 0x1000;
const SCRATCH: u64 = 0x8000;
const SCRATCH_LEN: usize = 0x1000;

/// Assemble a 32-bit snippet, run it through the interpreter (`CpuMode::Compat32`)
/// and Unicorn (`UC_MODE_32`), and assert identical final state with the given
/// undefined-flag mask. Panics with a precise divergence report on mismatch.
fn diff32(
    build: impl FnOnce(&mut CodeAssembler),
    init: impl FnOnce(&mut CpuSnapshot),
    dont_care: &[FlagName],
) {
    let mut asm = CodeAssembler::new(32).unwrap();
    build(&mut asm);
    let code = asm.assemble(ENTRY).unwrap();

    let mut cpu_init = CpuSnapshot {
        rip: ENTRY,
        ..Default::default()
    };
    init(&mut cpu_init);

    let input = VectorInput {
        cpu_init,
        mem_init: vec![
            MemChunk {
                addr: ENTRY,
                bytes: code,
                kind: MemKind::Ram,
            },
            MemChunk {
                addr: SCRATCH,
                bytes: vec![0u8; SCRATCH_LEN],
                kind: MemKind::Ram,
            },
        ],
        entry: ENTRY,
        run: RunSpec::UntilExit,
    };

    let interp = run_with_backend_mode(
        &input,
        Box::new(InterpreterBackend),
        GuestCpuFeatures::default(),
        CpuMode::Compat32,
    );
    let unicorn = UnicornOracle32.run(&input);
    if let Some(d) = compare(&unicorn, &interp, dont_care) {
        panic!("32-bit interpreter diverges from Unicorn UC_MODE_32:\n{d}");
    }
}

/// A mid-scratch address, a convenient initial ESP for 32-bit stack ops.
fn scratch_sp() -> u64 {
    SCRATCH + 0x800
}

// ---------------------------------------------------------------------------
// Mode-neutral cases — pass on pure task-197.1 plumbing (un-ignored). These are
// the proof the 32-bit lane runs vs Unicorn UC_MODE_32.
// ---------------------------------------------------------------------------

#[test]
fn add_carry_and_overflow_32() {
    diff32(
        |a| {
            a.mov(eax, 0x7FFF_FFFFi32).unwrap();
            a.add(eax, 1i32).unwrap(); // signed overflow: OF=1, SF=1
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn sub_borrow_sets_flags_32() {
    diff32(
        |a| {
            a.mov(eax, 0i32).unwrap();
            a.sub(eax, 1i32).unwrap(); // CF=1, SF=1, result 0xFFFFFFFF
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn logic_forces_cf_of_zero_32() {
    diff32(
        |a| {
            a.mov(eax, 0xF0F0i32).unwrap();
            a.and(eax, 0x0FF0i32).unwrap();
            a.or(eax, 0x0003i32).unwrap();
            a.xor(eax, 0x00FFi32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af],
    );
}

/// The headline 32-bit-mode case: `inc`/`dec` on a 32-bit register assemble to the
/// single-byte 0x40–0x4F opcodes (which are REX prefixes in long mode). The lifter
/// already lifts Inc/Dec, so this decodes and executes on plumbing alone — no
/// 197.2/197.3 semantics involved.
#[test]
fn inc_dec_short_forms_0x40_0x4f_32() {
    // Verify the encoding is genuinely the short form, then check semantics.
    let mut asm = CodeAssembler::new(32).unwrap();
    asm.inc(eax).unwrap();
    asm.dec(ecx).unwrap();
    let bytes = asm.assemble(ENTRY).unwrap();
    assert_eq!(bytes, vec![0x40, 0x49], "inc eax / dec ecx short forms");

    diff32(
        |a| {
            a.mov(eax, 0i32).unwrap();
            a.sub(eax, 1i32).unwrap(); // CF=1
            a.mov(ecx, 41i32).unwrap();
            a.inc(ecx).unwrap(); // 0x41: ecx=42, CF preserved
            a.inc(eax).unwrap(); // 0x40
            a.dec(ecx).unwrap(); // 0x49: ecx=41, CF preserved
            a.mov(edx, 0x7FFF_FFFFi32).unwrap();
            a.inc(edx).unwrap(); // signed overflow: OF=1
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// Every 32-bit register's inc/dec short form (0x40..0x47 / 0x48..0x4F), so the
/// whole opcode row decodes.
#[test]
fn inc_dec_all_regs_short_forms_32() {
    diff32(
        |a| {
            a.mov(eax, 1i32).unwrap();
            a.mov(ecx, 2i32).unwrap();
            a.mov(edx, 3i32).unwrap();
            a.mov(ebx, 4i32).unwrap();
            a.mov(esi, 5i32).unwrap();
            a.mov(edi, 6i32).unwrap();
            a.inc(eax).unwrap();
            a.inc(ecx).unwrap();
            a.inc(edx).unwrap();
            a.inc(ebx).unwrap();
            a.inc(esi).unwrap();
            a.inc(edi).unwrap();
            a.dec(eax).unwrap();
            a.dec(ecx).unwrap();
            a.dec(edx).unwrap();
            a.dec(ebx).unwrap();
            a.dec(esi).unwrap();
            a.dec(edi).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn movzx_movsx_32() {
    diff32(
        |a| {
            a.mov(ebx, 0x80i32).unwrap(); // bl = 0x80
            a.movzx(eax, bl).unwrap(); // 0x0000_0080
            a.movsx(ecx, bl).unwrap(); // 0xFFFF_FF80
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn setcc_from_compare_32() {
    diff32(
        |a| {
            a.mov(eax, 3i32).unwrap();
            a.cmp(eax, 5i32).unwrap(); // 3 < 5 -> below/less
            a.setb(bl).unwrap();
            a.setl(cl).unwrap();
            a.setg(dl).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn cmovcc_taken_and_not_taken_32() {
    diff32(
        |a| {
            a.mov(eax, 1i32).unwrap();
            a.mov(ecx, 0x1111i32).unwrap();
            a.mov(edx, 0x2222i32).unwrap();
            a.cmp(eax, 0i32).unwrap();
            a.cmovg(ecx, edx).unwrap(); // taken
            a.cmovl(ecx, eax).unwrap(); // not taken
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

#[test]
fn shift_and_rotate_32() {
    diff32(
        |a| {
            a.mov(eax, 0xC000_0001u32 as i32).unwrap();
            a.shl(eax, 1i32).unwrap();
            a.mov(ebx, 0x0000_0003i32).unwrap();
            a.shr(ebx, 1i32).unwrap();
            a.mov(ecx, 0x8000_0004u32 as i32).unwrap();
            a.sar(ecx, 1i32).unwrap();
            a.mov(edx, 0x8000_0001u32 as i32).unwrap();
            a.rol(edx, 1i32).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af],
    );
}

#[test]
fn mul_imul_32() {
    diff32(
        |a| {
            a.mov(eax, 0x0012_3456i32).unwrap();
            a.mov(ebx, 0x0000_789Ai32).unwrap();
            a.mul(ebx).unwrap();
            a.mov(eax, 50_000i32).unwrap();
            a.mov(ecx, 50_000i32).unwrap();
            a.imul_2(eax, ecx).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[FlagName::Af, FlagName::Sf, FlagName::Zf, FlagName::Pf],
    );
}

#[test]
fn div_idiv_32() {
    diff32(
        |a| {
            a.mov(edx, 0i32).unwrap();
            a.mov(eax, 1_000_003i32).unwrap();
            a.mov(ecx, 7i32).unwrap();
            a.div(ecx).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[
            FlagName::Cf,
            FlagName::Pf,
            FlagName::Af,
            FlagName::Zf,
            FlagName::Sf,
            FlagName::Of,
        ],
    );
}

/// A short countdown loop: relative jcc within a 32-bit flat block. Control-flow
/// stays inside the low 32-bit address space, so no EIP-wrap semantics are needed.
#[test]
fn conditional_countdown_loop_32() {
    diff32(
        |a| {
            let mut top = a.create_label();
            a.mov(ecx, 5i32).unwrap();
            a.mov(eax, 0i32).unwrap();
            a.set_label(&mut top).unwrap();
            a.add(eax, ecx).unwrap();
            a.sub(ecx, 1i32).unwrap();
            a.jnz(top).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// 32-bit SSE (mode-neutral encodings): pack a couple of values and run logic ops.
#[test]
fn sse_logic_32() {
    diff32(
        |a| {
            a.mov(eax, 0x1122_3344u32 as i32).unwrap();
            a.movd(xmm0, eax).unwrap();
            a.mov(ebx, 0xAABB_CCDDu32 as i32).unwrap();
            a.movd(xmm1, ebx).unwrap();
            a.pxor(xmm2, xmm2).unwrap();
            a.por(xmm2, xmm0).unwrap();
            a.pand(xmm2, xmm1).unwrap();
            a.movd(ecx, xmm2).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// 32-bit `lea` with base+index*scale+disp — a flat-mode effective address that
/// stays 32-bit. Address computation is mode-neutral here (no wrap), so this passes
/// on plumbing.
#[test]
fn lea_base_index_scale_disp_32() {
    diff32(
        |a| {
            a.mov(ebx, 0x10i32).unwrap();
            a.mov(ecx, 0x3i32).unwrap();
            a.lea(eax, dword_ptr(ebx + ecx * 4 + 8)).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

// ---------------------------------------------------------------------------
// 32-bit-only cases — KNOWN GAPS. These depend on execution semantics owned by
// sibling branches (task-197.2: address wrap / 67h, task-197.3: EIP wrap / stack
// widths). Not on this branch, so ignored with an explicit task tag. Integration
// un-ignores them after the semantics merge. See the task notes' KNOWN-GAPS list.
// ---------------------------------------------------------------------------

/// 32-bit push/pop with an explicit 32-bit operand: 4 bytes move on the stack in
/// either mode, and the stack pointer stays in range, so this is mode-neutral and
/// passes on plumbing. (The genuine 197.3 gap — default push width and ESP masking —
/// shows up in `call_ret_32`, where the return-address width diverges.)
#[test]
fn push_pop_roundtrip_32() {
    diff32(
        |a| {
            a.mov(eax, 0xDEAD_BEEFu32 as i32).unwrap();
            a.push(eax).unwrap();
            a.pop(ebx).unwrap();
            a.hlt().unwrap();
        },
        |cpu| cpu.gpr[4] = scratch_sp(), // esp mid-scratch
        &[],
    );
}

/// `call`/`ret` in 32-bit push a 32-bit return EIP and pop it. Stack-width + return
/// address semantics are task-197.3.
#[test]
#[ignore = "197.3: 32-bit call/ret (4-byte return EIP on the stack)"]
fn call_ret_32() {
    diff32(
        |a| {
            let mut target = a.create_label();
            let mut done = a.create_label();
            a.call(target).unwrap();
            a.jmp(done).unwrap();
            a.set_label(&mut target).unwrap();
            a.mov(eax, 0x1234i32).unwrap();
            a.ret().unwrap();
            a.set_label(&mut done).unwrap();
            a.hlt().unwrap();
        },
        |cpu| cpu.gpr[4] = scratch_sp(),
        &[],
    );
}

/// 16-bit stack ops (`push ax`/`pop bx`) move the stack pointer by 2 in either mode
/// (the operand size, not the mode, sets the width here), and the pointer stays in
/// range — so this is mode-neutral and passes on plumbing.
#[test]
fn push_pop_16bit_32() {
    diff32(
        |a| {
            a.mov(eax, 0xBEEFi32).unwrap();
            a.push(ax).unwrap();
            a.pop(bx).unwrap();
            a.hlt().unwrap();
        },
        |cpu| cpu.gpr[4] = scratch_sp(),
        &[],
    );
}

/// A 67h address-size override selecting 16-bit addressing, *in range* (no wrap):
/// the effective address is the 16-bit register value, which the interpreter already
/// computes correctly on plumbing. Kept un-ignored as coverage that the 67h form
/// decodes and the base-register read is mode-correct.
#[test]
fn addr16_override_67h_in_range_32() {
    diff32(
        |a| {
            // Seed the scratch cell, then load it back through a 16-bit effective
            // address (67h prefix). iced emits the 67h form for a 16-bit addressing
            // expression; [bx] = 0x8000 = SCRATCH, below 64 KiB, so no wrap needed.
            a.mov(dword_ptr(SCRATCH), 0x1234_5678u32 as i32).unwrap();
            a.mov(bx, (SCRATCH as u16) as i32).unwrap();
            a.mov(eax, dword_ptr(bx)).unwrap(); // 67h: [bx]
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// A 67h 16-bit effective address that *wraps within 64 KiB*: `[bx+si]` where the
/// sum exceeds 0xFFFF must truncate to 16 bits (wrap), not carry into a larger
/// address. This is the 16-bit-addressing wrap semantics task-197.2 owns.
#[test]
#[ignore = "197.2: 67h 16-bit address wrap within 64 KiB"]
fn addr16_override_67h_wrap_32() {
    diff32(
        |a| {
            // bx=0xFFFF, si=0x8001 → [bx+si] = 0x1_8000 wraps to 0x8000 = SCRATCH in
            // 16-bit addressing. Without wrap the effective address is 0x18000 (unmapped).
            a.mov(dword_ptr(SCRATCH), 0xABCD_1234u32 as i32).unwrap();
            a.mov(bx, 0xFFFFi32).unwrap();
            a.mov(si, 0x8001i32).unwrap();
            a.mov(eax, dword_ptr(bx + si)).unwrap(); // 67h: [bx+si], wraps to SCRATCH
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}

/// Address wrap at 4 GiB: a base+disp that carries past 0xFFFF_FFFF wraps to a low
/// address in 32-bit mode (not extending into the 64-bit space). The effective-
/// address seam already truncates to 32 bits under `CpuMode::Compat32`, so this
/// passes on plumbing — un-ignored as live coverage of the wrap. (The 67h *16-bit*
/// wrap below is the piece that still needs 197.2 semantics.)
#[test]
fn addr_wrap_4gib_32() {
    diff32(
        |a| {
            // ebx near the top of the 32-bit space; a positive disp wraps to low mem.
            a.mov(dword_ptr(SCRATCH), 0xCAFE_BABEu32 as i32).unwrap();
            a.mov(ebx, 0xFFFF_F000u32 as i32).unwrap();
            // effective = 0xFFFF_F000 + disp wraps around 2^32 to exactly SCRATCH.
            let disp = (SCRATCH as u32).wrapping_sub(0xFFFF_F000);
            a.mov(eax, dword_ptr(ebx + disp)).unwrap();
            a.hlt().unwrap();
        },
        |_| {},
        &[],
    );
}
