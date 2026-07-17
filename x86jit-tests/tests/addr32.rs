//! 32-bit (`Compat32`) effective-address acceptance (task-197.2). Minimal,
//! self-contained differential plumbing: assemble a single `hlt`-terminated 32-bit
//! block, run it through x86jit's interpreter and JIT under `CpuMode::Compat32`, and
//! compare the final GPR state against Unicorn in `UC_MODE_32`. Kept local (not on
//! the general 32-bit harness, task-197.5) so the cases can later be ported onto that
//! lane.
//!
//! Scope: effective-address arithmetic only — 32-bit wrap, the 0x67 16-bit addressing
//! forms, and `lea` truncation / segment-base handling. Snippets use only
//! mov/add/lea/load/store (no push/pop/call/branch) because stack-width and EIP-wrap
//! semantics are task-197.3's territory.

#![cfg(feature = "unicorn")]

use iced_x86::code_asm::*;

use unicorn_engine::unicorn_const::{Arch, Mode, Prot as UcProt};
use unicorn_engine::{RegisterX86, Unicorn};

use x86jit_core::jit_abi::run_compiled;
use x86jit_core::lift::{lift_block, CpuMode, FetchAddr};
use x86jit_core::{
    CachedBlock, Exit, InterpreterBackend, Prot, RegionKind, StepResult, Vm, VmConfig,
};
use x86jit_cranelift::JitBackend;

const CODE: u64 = 0x1000;
const CODE_LEN: u64 = 0x1000;
/// Scratch RW data page. Deliberately low (< 4 GiB) so a 32-bit effective address
/// can reach it; the wrap cases arrange for a truncated address to land here.
const SCRATCH: u64 = 0x8000;
const SCRATCH_LEN: u64 = 0x1000;

/// x86-encoding GPR slot order → Unicorn 32-bit register id.
const E_REGS: [RegisterX86; 8] = [
    RegisterX86::EAX,
    RegisterX86::ECX,
    RegisterX86::EDX,
    RegisterX86::EBX,
    RegisterX86::ESP,
    RegisterX86::EBP,
    RegisterX86::ESI,
    RegisterX86::EDI,
];

/// Initial guest state a case sets up: the low 32 bits of the 8 legacy GPRs plus the
/// FS base (for the seg-prefixed `lea` case). Data written into the scratch page.
#[derive(Clone, Default)]
struct Init {
    gpr: [u32; 8],
    fs_base: u64,
    /// (offset-from-SCRATCH, dword value) writes into the scratch page before the run.
    data: Vec<(u64, u32)>,
}

/// Run one 32-bit snippet three ways (interp, JIT, Unicorn) and assert the engines
/// agree with Unicorn on the low 32 bits of every GPR. `build` assembles the block;
/// it MUST end in `hlt`.
fn diff32(build: impl Fn(&mut CodeAssembler), init: Init) {
    let mut asm = CodeAssembler::new(32).unwrap();
    build(&mut asm);
    let code = asm.assemble(CODE).unwrap();
    diff32_bytes(&code, init);
}

/// As [`diff32`] but from pre-assembled bytes — for addressing forms iced's `code_asm`
/// won't emit directly (e.g. the 0x67 `[disp16]` absolute).
fn diff32_bytes(code: &[u8], init: Init) {
    let code = code.to_vec();
    let x86jit = |jit: bool| -> [u64; 16] {
        let backend: Box<dyn x86jit_core::Backend> = if jit {
            Box::new(JitBackend::new())
        } else {
            Box::new(InterpreterBackend)
        };
        let mut vm = Vm::with_backend(VmConfig::flat(0x1_0000), backend);
        vm.map(CODE, CODE_LEN as usize, Prot::RX, RegionKind::Ram)
            .unwrap();
        vm.map(SCRATCH, SCRATCH_LEN as usize, Prot::RW, RegionKind::Ram)
            .unwrap();
        vm.write_bytes(CODE, &code).unwrap();
        for (off, v) in &init.data {
            vm.write_bytes(SCRATCH + off, &v.to_le_bytes()).unwrap();
        }

        let mut vcpu = vm.new_vcpu();
        for (i, &v) in init.gpr.iter().enumerate() {
            vcpu.cpu.gpr[i] = v as u64;
        }
        vcpu.cpu.fs_base = init.fs_base;
        vcpu.cpu.rip = CODE;

        let ir = lift_block(&vm.mem, FetchAddr::flat(CODE), CpuMode::Compat32)
            .expect("lift 32-bit block");
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
            unsafe { run_compiled(entry, &mut vcpu.cpu, &vm.mem, CpuMode::Compat32) }
        } else {
            let mut scratch = Vec::new();
            x86jit_core::interp::interpret_block(&ir, &mut vcpu.cpu, &vm.mem, &mut scratch)
        };
        match result {
            StepResult::Exit(Exit::Hlt) => {}
            _ => panic!(
                "block must terminate at hlt (jit={jit}); rip={:#x}",
                vcpu.cpu.rip
            ),
        }
        vcpu.cpu.gpr
    };

    let interp = x86jit(false);
    let jit = x86jit(true);

    // Unicorn oracle, 32-bit protected/flat.
    let mut uc = Unicorn::new(Arch::X86, Mode::MODE_32).expect("open unicorn x86-32");
    uc.mem_map(CODE, CODE_LEN, UcProt::ALL).expect("map code");
    uc.mem_map(SCRATCH, SCRATCH_LEN, UcProt::ALL)
        .expect("map scratch");
    uc.mem_write(CODE, &code).expect("write code");
    for (off, v) in &init.data {
        uc.mem_write(SCRATCH + off, &v.to_le_bytes())
            .expect("write data");
    }
    for (reg, &v) in E_REGS.iter().zip(&init.gpr) {
        uc.reg_write(*reg, v as u64).unwrap();
    }
    uc.reg_write(RegisterX86::FS_BASE, init.fs_base).unwrap();

    // Stop before the terminating hlt (privileged): a code hook halts on the 0xf4 byte.
    let hlt_off = code
        .iter()
        .position(|&b| b == 0xf4)
        .expect("snippet has hlt") as u64;
    let hlt_addr = CODE + hlt_off;
    uc.add_code_hook(CODE, u64::MAX, move |uc, addr, _size| {
        if addr == hlt_addr {
            let _ = uc.emu_stop();
        }
    })
    .expect("install hlt hook");
    uc.emu_start(CODE, u64::MAX, 0, 100).expect("unicorn run");

    let mut ref_gpr = [0u64; 16];
    for (slot, reg) in ref_gpr.iter_mut().zip(&E_REGS) {
        *slot = uc.reg_read(*reg).unwrap() & 0xFFFF_FFFF;
    }

    for i in 0..8 {
        let m = 0xFFFF_FFFFu64;
        assert_eq!(
            interp[i] & m,
            ref_gpr[i],
            "interp gpr[{i}] mismatch vs unicorn"
        );
        assert_eq!(jit[i] & m, ref_gpr[i], "jit gpr[{i}] mismatch vs unicorn");
    }
}

/// AC#1: a 32-bit effective address wraps modulo 2^32. `EBX = 0xFFFF_FFFF`, load
/// `[ebx + (SCRATCH + 1)]` → `(0xFFFF_FFFF + SCRATCH + 1) mod 2^32 = SCRATCH`, reading
/// the dword we planted there. The un-truncated 64-bit sum would be unmapped.
#[test]
fn addr32_wraps_at_4gib() {
    diff32(
        |a| {
            a.mov(eax, dword_ptr(ebx + (SCRATCH as i32 + 1))).unwrap();
            a.hlt().unwrap();
        },
        Init {
            gpr: {
                let mut g = [0u32; 8];
                g[3] = 0xFFFF_FFFF; // EBX
                g
            },
            data: vec![(0, 0xDEAD_BEEF)],
            ..Default::default()
        },
    );
}

/// AC#1: negative-displacement wrap. `EBX = 4`, load `[ebx + (SCRATCH - 4)]`. The
/// displacement `SCRATCH - 4` is a large positive disp32 whose 32-bit sum with EBX is
/// exactly SCRATCH; a signed/64-bit miscompute would land elsewhere.
#[test]
fn addr32_negative_disp_wrap() {
    diff32(
        |a| {
            a.mov(eax, dword_ptr(ebx + (SCRATCH as i32 - 4))).unwrap();
            a.hlt().unwrap();
        },
        Init {
            gpr: {
                let mut g = [0u32; 8];
                g[3] = 4; // EBX
                g
            },
            data: vec![(0, 0x1234_5678)],
            ..Default::default()
        },
    );
}

/// AC#1: base + scaled index + disp, all truncated together. `EBX = 0xFFFF_FFF0`,
/// `ECX = 4`, scale 4, disp = SCRATCH + 0x10 → wraps to SCRATCH + 0x20.
#[test]
fn addr32_base_index_scale_wrap() {
    diff32(
        |a| {
            a.mov(eax, dword_ptr(ebx + ecx * 4 + (SCRATCH as i32 + 0x10)))
                .unwrap();
            a.hlt().unwrap();
        },
        Init {
            gpr: {
                let mut g = [0u32; 8];
                g[3] = 0xFFFF_FFF0; // EBX  (base)
                g[1] = 4; // ECX (index*4 = 16)
                g
            },
            // truncated addr = (0xFFFF_FFF0 + 16 + SCRATCH + 0x10) mod 2^32
            //                = SCRATCH + 0x20
            data: vec![(0x20, 0xCAFE_F00D)],
            ..Default::default()
        },
    );
}

/// AC#2: 0x67 selects 16-bit addressing. `[bx + si]` wraps modulo 2^16. `BX = 0x9000`,
/// `SI = 0xF000` → `(0x9000 + 0xF000) mod 2^16 = 0x8000 = SCRATCH`.
#[test]
fn addr16_bx_si_wraps_mod_64k() {
    diff32(
        |a| {
            a.mov(eax, dword_ptr(bx + si)).unwrap();
            a.hlt().unwrap();
        },
        Init {
            gpr: {
                let mut g = [0u32; 8];
                g[3] = 0x9000; // BX  (low 16 of EBX)
                g[6] = 0xF000; // SI  (low 16 of ESI)
                g
            },
            data: vec![(0, 0x0BAD_F00D)],
            ..Default::default()
        },
    );
}

/// AC#2: 0x67 `[bp + di + disp8]`. `BP = 0x7000`, `DI = 0x0FF8`, disp = 8 →
/// `0x7000 + 0x0FF8 + 8 = 0x8000 = SCRATCH`. (`bp`-based 16-bit forms are still a flat
/// linear address here — no stack segment.)
#[test]
fn addr16_bp_di_disp() {
    diff32(
        |a| {
            a.mov(eax, dword_ptr(bp + di + 8)).unwrap();
            a.hlt().unwrap();
        },
        Init {
            gpr: {
                let mut g = [0u32; 8];
                g[5] = 0x7000; // BP
                g[7] = 0x0FF8; // DI
                g
            },
            data: vec![(0, 0xFEED_FACE)],
            ..Default::default()
        },
    );
}

/// AC#2: 0x67 `[disp16]` absolute (no base/index, no SIB). iced's `code_asm` won't
/// emit the 16-bit absolute form for a 32-bit assembler, so hand-encode
/// `67 8b 06 <disp16>` = `mov eax, [disp16]`, then `hlt`. disp16 = SCRATCH (0x8000).
#[test]
fn addr16_disp16_absolute() {
    let disp = (SCRATCH as u16).to_le_bytes();
    let code = [0x67, 0x8b, 0x06, disp[0], disp[1], 0xf4];
    diff32_bytes(
        &code,
        Init {
            data: vec![(0, 0x00C0_FFEE)],
            ..Default::default()
        },
    );
}

/// AC#3: `lea` honours 32-bit address-size truncation and never adds a segment base.
/// `lea eax, [ebx + ecx*4 + 8]` with EBX = 0xFFFF_FFFC, ECX = 1 → EAX =
/// (0xFFFF_FFFC + 4 + 8) mod 2^32 = 8.
#[test]
fn lea32_truncates_address() {
    diff32(
        |a| {
            a.lea(eax, dword_ptr(ebx + ecx * 4 + 8)).unwrap();
            a.hlt().unwrap();
        },
        Init {
            gpr: {
                let mut g = [0u32; 8];
                g[3] = 0xFFFF_FFFC; // EBX
                g[1] = 1; // ECX
                g
            },
            ..Default::default()
        },
    );
}

/// AC#3: seg-prefixed `lea` ignores the segment base. `lea eax, fs:[ebx]` is
/// `EAX = EBX`, NOT `EBX + fs_base`, even with a live nonzero FS base.
#[test]
fn lea32_ignores_segment_base() {
    diff32(
        |a| {
            a.lea(eax, dword_ptr(ebx).fs()).unwrap();
            a.hlt().unwrap();
        },
        Init {
            gpr: {
                let mut g = [0u32; 8];
                g[3] = 0x2000; // EBX; expect EAX = 0x2000
                g
            },
            fs_base: 0x5000, // the old (buggy) add would show as 0x7000
            ..Default::default()
        },
    );
}
