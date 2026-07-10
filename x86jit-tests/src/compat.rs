//! ISA compatibility probe (OCI-0.T1): measure, *mechanically*, which x86-64
//! instruction forms the lifter (`x86jit_core::lift`) actually handles, bucketed by
//! instruction-set generation (v1/v2/v3/v4 + x87/MMX). The map is computed by
//! probing the real lifter — never hand-written prose, which rots immediately (the
//! in-tree CPUID comment was already wrong; see `oci-plan.md` §OCI-0).
//!
//! Method: for every `iced_x86::Code` valid in 64-bit mode and in scope (its CPUID
//! features map to a generation we model), synthesize a canonical instruction with
//! templated operands, encode it, feed the bytes (plus a `ret` terminator) to
//! `lift_block`, and classify Lifted / Unsupported / Unencodable.

use std::collections::BTreeMap;

use iced_x86::{Code, CpuidFeature, Encoder, Instruction, OpCodeOperandKind, OpKind, Register};
use serde::{Deserialize, Serialize};
use x86jit_core::lift::{lift_block, CpuMode, LiftError};
use x86jit_core::{Memory, MemoryModel, Prot, RegionKind};

/// Instruction-set generation buckets we model (x86-64 userland).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Gen {
    /// x86-64-v1 baseline: scalar 64-bit + SSE + SSE2.
    V1,
    /// x86-64-v2: SSE3, SSSE3, SSE4.1, SSE4.2, POPCNT, CMPXCHG16B, LAHF/SAHF.
    V2,
    /// x86-64-v3: AVX, AVX2, BMI1/2, FMA, F16C, LZCNT, MOVBE.
    V3,
    /// x86-64-v4: AVX-512 F/BW/DQ/VL/CD (task-169; the in-progress AVX-512 lift).
    V4,
    /// x87 FPU (fidelity note: implemented f64-backed, not true 80-bit).
    X87,
    /// Legacy MMX.
    Mmx,
}

impl Gen {
    pub fn label(self) -> &'static str {
        match self {
            Gen::V1 => "x86-64-v1",
            Gen::V2 => "x86-64-v2",
            Gen::V3 => "x86-64-v3",
            Gen::V4 => "x86-64-v4",
            Gen::X87 => "x87",
            Gen::Mmx => "mmx",
        }
    }
}

/// Map a single CPUID feature to the generation it belongs to, or `None` if it is
/// out of scope (AVX-512, privileged/system, vendor extensions — deliberately not
/// modeled here; those get their own brief when a target image demands them).
fn feature_gen(f: CpuidFeature) -> Option<Gen> {
    use CpuidFeature::*;
    Some(match f {
        // Baseline scalar userland — a v1 CPU without SIMD features.
        INTEL8086 | INTEL8086_ONLY | INTEL186 | INTEL286 | INTEL286_ONLY | INTEL386
        | INTEL386_ONLY | INTEL386_A0_ONLY | INTEL486 | INTEL486_A_ONLY | CMOV | CLFSH
        | CLFLUSHOPT | CPUID | PAUSE | MULTIBYTENOP | FXSR | SSE | SSE2 | SYSCALL | SEP
        | RDTSCP | SMAP => Gen::V1,
        // x86-64-v2.
        SSE3 | SSSE3 | SSE4_1 | SSE4_2 | POPCNT | CMPXCHG16B | MOVBE => Gen::V2,
        // x86-64-v3.
        AVX | AVX2 | BMI1 | BMI2 | FMA | F16C | LZCNT => Gen::V3,
        // x86-64-v4 (AVX-512 base set; sub-extensions like VBMI/VNNI stay out of scope,
        // so a code needing them is skipped by the all-features-modeled rule).
        AVX512F | AVX512BW | AVX512DQ | AVX512VL | AVX512CD => Gen::V4,
        // x87 FPU (iced has no single X87 feature — the FPU* variants tag it).
        FPU | FPU287 | FPU287XL_ONLY | FPU387 | FPU387SL_ONLY => Gen::X87,
        MMX => Gen::Mmx,
        // Everything else (AVX-512 family, SSE4A/FMA4 AMD, privileged, TSX, SHA, AES,
        // vendor) is out of scope for this map.
        _ => return None,
    })
}

/// Generation of a whole instruction: in scope only if *all* its CPUID features are
/// modeled; the generation is the highest among them (an SSE4.1 instruction that
/// also lists SSE2 is v2). MOVBE is v2 but pairs with a baseline feature — the max
/// rule handles it. Returns `None` for out-of-scope instructions.
fn code_gen(code: Code) -> Option<Gen> {
    let feats = code.cpuid_features();
    if feats.is_empty() {
        return None;
    }
    let mut acc: Option<Gen> = None;
    for &f in feats {
        let g = feature_gen(f)?; // any out-of-scope feature ⇒ whole insn out of scope
        acc = Some(match acc {
            Some(prev) if prev >= g => prev,
            _ => g,
        });
    }
    acc
}

/// Result of probing one instruction form.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Probe {
    /// The lifter produced IR for it.
    Lifted,
    /// The lifter returned `Unsupported` — a real gap.
    Unsupported,
    /// We couldn't synthesize/encode a canonical form (exotic operand kind); not
    /// counted for/against coverage, but logged so the scope is honest.
    Unencodable,
}

const SCRATCH_BASE: u64 = 0x1000;

/// Synthesize + encode a canonical instance of `code`, lift it, and classify.
/// Returns `None` if the code is out of scope or not valid in 64-bit mode.
pub fn probe_code(code: Code) -> Option<Probe> {
    let info = code.op_code();
    if !info.mode64() || code == Code::INVALID {
        return None;
    }
    code_gen(code)?; // scope gate

    let mut instr = Instruction::default();
    instr.set_code(code);
    // iced needs the operand count set before per-operand setters take effect for
    // some forms; the OpCodeInfo drives how many operands to template.
    let op_count = info.op_count();
    for i in 0..op_count {
        if template_operand(&mut instr, i, info.op_kind(i)).is_err() {
            return Some(Probe::Unencodable);
        }
    }

    let mut enc = Encoder::new(64);
    let bytes = match enc.encode(&instr, SCRATCH_BASE) {
        Ok(_) => enc.take_buffer(),
        Err(_) => return Some(Probe::Unencodable),
    };
    if bytes.is_empty() {
        return Some(Probe::Unencodable);
    }

    // Append a `ret` (0xC3) so a non-terminator probe instruction still closes the
    // block; a terminator probe (jmp/ret/…) ends it itself.
    let mut prog = bytes.clone();
    prog.push(0xC3);
    let mut mem = Memory::new(MemoryModel::Flat { size: 0x4000 });
    // write_bytes needs a mapped region (it bypasses guest Prot but the region must
    // exist); map the whole scratch buffer as executable RAM.
    if mem.map(0, 0x4000, Prot::RX, RegionKind::Ram).is_err() {
        return Some(Probe::Unencodable);
    }
    if mem.write_bytes(SCRATCH_BASE, &prog).is_err() {
        return Some(Probe::Unencodable);
    }
    match lift_block(&mem, SCRATCH_BASE, CpuMode::Long64) {
        Ok(_) => Some(Probe::Lifted),
        Err(LiftError::Unsupported { addr, .. }) if addr == SCRATCH_BASE => {
            Some(Probe::Unsupported)
        }
        // Unsupported past the probe insn (shouldn't happen with a plain ret), or a
        // decode error on our own bytes: treat as unencodable noise, not a gap.
        Err(_) => Some(Probe::Unencodable),
    }
}

/// Pick registers by operand index so a 2-operand form gets distinct registers.
fn nth<const N: usize>(regs: [Register; N], i: u32) -> Register {
    regs[(i as usize) % N]
}

/// Set operand `i` of `instr` to a canonical value for `kind`. `Err(())` means the
/// operand kind is exotic/unsupported by this templater (⇒ Unencodable).
fn template_operand(instr: &mut Instruction, i: u32, kind: OpCodeOperandKind) -> Result<(), ()> {
    use OpCodeOperandKind::*;
    // Register-form operands (including the register alternative of `*_or_mem`).
    let reg = |instr: &mut Instruction, r: Register| {
        instr.set_op_kind(i, OpKind::Register);
        instr.set_op_register(i, r);
    };
    match kind {
        r8_reg | r8_opcode | r8_or_mem => reg(
            instr,
            nth([Register::AL, Register::CL, Register::DL, Register::BL], i),
        ),
        r16_reg | r16_reg_mem | r16_rm | r16_opcode | r16_or_mem => reg(
            instr,
            nth([Register::AX, Register::CX, Register::DX, Register::BX], i),
        ),
        r32_reg | r32_reg_mem | r32_rm | r32_opcode | r32_vvvv | r32_or_mem | r32_or_mem_mpx => {
            reg(
                instr,
                nth(
                    [Register::EAX, Register::ECX, Register::EDX, Register::EBX],
                    i,
                ),
            )
        }
        r64_reg | r64_reg_mem | r64_rm | r64_opcode | r64_vvvv | r64_or_mem | r64_or_mem_mpx => {
            reg(
                instr,
                nth(
                    [Register::RAX, Register::RCX, Register::RDX, Register::RBX],
                    i,
                ),
            )
        }
        xmm_reg | xmm_rm | xmm_vvvv | xmm_is4 | xmm_is5 | xmmp3_vvvv | xmm_or_mem => reg(
            instr,
            nth(
                [
                    Register::XMM0,
                    Register::XMM1,
                    Register::XMM2,
                    Register::XMM3,
                ],
                i,
            ),
        ),
        ymm_reg | ymm_rm | ymm_vvvv | ymm_is4 | ymm_is5 | ymm_or_mem => reg(
            instr,
            nth(
                [
                    Register::YMM0,
                    Register::YMM1,
                    Register::YMM2,
                    Register::YMM3,
                ],
                i,
            ),
        ),
        mm_reg | mm_rm | mm_or_mem => reg(
            instr,
            nth(
                [Register::MM0, Register::MM1, Register::MM2, Register::MM3],
                i,
            ),
        ),
        // Fixed implicit registers.
        al => reg(instr, Register::AL),
        cl => reg(instr, Register::CL),
        ax => reg(instr, Register::AX),
        dx => reg(instr, Register::DX),
        eax => reg(instr, Register::EAX),
        rax => reg(instr, Register::RAX),
        st0 => reg(instr, Register::ST0),
        sti_opcode => reg(instr, Register::ST1),
        // Pure memory operands.
        mem | mem_offs => {
            instr.set_op_kind(i, OpKind::Memory);
            instr.set_memory_base(Register::RAX);
            instr.set_memory_displacement64(0x40);
        }
        // Immediates.
        imm8 | imm8_const_1 | imm8sex16 | imm8sex32 | imm8sex64 | imm4_m2z => {
            instr.set_op_kind(i, OpKind::Immediate8);
            instr.set_immediate8(1);
        }
        // Anything else (mask regs, zmm, vsib, seg/cr/dr/tr/bnd, moffs, rel, wider
        // immediates, is4 for legacy) — exotic or out of our v1..v3 focus.
        _ => return Err(()),
    }
    Ok(())
}

// --- coverage aggregation + serialized artifacts ---

/// Per-generation counts + the concrete missing/partial code lists (the machine-
/// readable artifact, `backlog/docs/compat/coverage.json`).
#[derive(Serialize, Deserialize, Default)]
pub struct GenCoverage {
    pub lifted: u32,
    pub unsupported: u32,
    pub unencodable: u32,
    /// Mnemonic-ish `Code` names that probed Unsupported (the gap list).
    pub missing: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Coverage {
    pub generations: BTreeMap<String, GenCoverage>,
}

/// Probe the whole in-scope ISA and aggregate.
pub fn compute_coverage() -> Coverage {
    let mut cov = Coverage::default();
    for code in Code::values() {
        let Some(g) = code_gen(code) else { continue };
        if !code.op_code().mode64() {
            continue;
        }
        let Some(p) = probe_code(code) else { continue };
        let entry = cov.generations.entry(g.label().to_string()).or_default();
        match p {
            Probe::Lifted => entry.lifted += 1,
            Probe::Unsupported => {
                entry.unsupported += 1;
                entry.missing.push(format!("{code:?}"));
            }
            Probe::Unencodable => entry.unencodable += 1,
        }
    }
    for gc in cov.generations.values_mut() {
        gc.missing.sort();
    }
    cov
}

// --- artifacts: machine-readable JSON + human dashboard ---

/// Directory holding the checked-in compat artifacts (`backlog/docs/compat/`).
pub fn artifact_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("backlog")
        .join("docs")
        .join("compat")
}

impl Coverage {
    /// Pretty JSON for `backlog/docs/compat/coverage.json` (stable key order via BTreeMap).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("serialize coverage") + "\n"
    }

    /// The human dashboard `backlog/docs/compat/isa-coverage.md`: one row per generation
    /// with the lifted/missing split and percentage, plus the concrete gap lists.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        // Backlog.md doc frontmatter so the generated dashboard is app-listed like the
        // curated docs. `created_date` is fixed (not "now") so regeneration is stable —
        // the bytes must match the checked-in file. `id` is stable too.
        s.push_str(
            "---\nid: doc-24\ntitle: 'ISA compatibility coverage'\ntype: other\n\
             created_date: '2026-07-06 11:25'\n---\n\n",
        );
        s.push_str("# ISA compatibility coverage\n\n");
        s.push_str(
            "**Generated** by `cargo run -p x86jit-tests --bin compat -- --write` — do NOT edit \
             by hand. Measured by probing the real lifter (`x86jit-tests/src/compat.rs`): a \
             canonical instance of every in-scope `iced_x86::Code` is encoded and fed to \
             `lift_block`. `lifted`/`missing` are of the *encodable* forms; `unencodable` are \
             exotic operand shapes the probe can't synthesize (not counted). Kept honest by the \
             `compat_map_is_current` test. See `backlog/docs/design/oci-plan.md` §OCI-0.\n\n",
        );
        s.push_str("| generation | lifted | missing | % of encodable | unencodable |\n");
        s.push_str("|---|---:|---:|---:|---:|\n");
        for (g, c) in &self.generations {
            let known = c.lifted + c.unsupported;
            let pct = if known > 0 {
                100.0 * c.lifted as f64 / known as f64
            } else {
                0.0
            };
            s.push_str(&format!(
                "| {g} | {} | {} | {pct:.0}% | {} |\n",
                c.lifted, c.unsupported, c.unencodable
            ));
        }
        for (g, c) in &self.generations {
            if c.missing.is_empty() {
                continue;
            }
            s.push_str(&format!("\n## {g} — missing ({})\n\n", c.missing.len()));
            for m in &c.missing {
                s.push_str(&format!("- `{m}`\n"));
            }
        }
        s
    }

    /// Write both artifacts into `backlog/docs/compat/`.
    pub fn write_artifacts(&self) -> std::io::Result<()> {
        let dir = artifact_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("coverage.json"), self.to_json())?;
        std::fs::write(dir.join("isa-coverage.md"), self.to_markdown())?;
        Ok(())
    }

    /// Load the checked-in JSON (for the staleness test).
    pub fn load_checked_in() -> std::io::Result<Coverage> {
        let text = std::fs::read_to_string(artifact_dir().join("coverage.json"))?;
        Ok(serde_json::from_str(&text).expect("parse coverage.json"))
    }
}

// --- CPUID ⇄ coverage consistency (OCI-0.T2) ---

/// The SIMD/legacy features leaf-1 CPUID currently advertises, read straight from
/// `cpuid_run` (the single source both interp and JIT use). Baseline scalar bits
/// (FPU/TSC/CX8/CMOV/FXSR) are not returned — they aren't gated feature paths a
/// guest branches on into unimplemented SIMD.
pub fn advertised_simd_features() -> Vec<CpuidFeature> {
    use x86jit_core::state::CpuState;
    let mut cpu = CpuState::new();
    cpu.gpr[0] = 1; // leaf 1 in RAX
    x86jit_core::interp::cpuid_run(&mut cpu);
    let ecx = cpu.gpr[1] as u32; // RCX
    let edx = cpu.gpr[2] as u32; // RDX
    use CpuidFeature::*;
    let mut v = Vec::new();
    for (bit, feat) in [(23u32, MMX), (25, SSE), (26, SSE2)] {
        if edx & (1 << bit) != 0 {
            v.push(feat);
        }
    }
    for (bit, feat) in [
        (0u32, SSE3),
        (9, SSSE3),
        (19, SSE4_1),
        (20, SSE4_2),
        (23, POPCNT),
    ] {
        if ecx & (1 << bit) != 0 {
            v.push(feat);
        }
    }
    v
}

/// Probe every in-scope Code tagged with `target` and report (lifted count, sorted
/// missing Code names).
pub fn feature_coverage(target: CpuidFeature) -> (u32, Vec<String>) {
    let mut lifted = 0;
    let mut missing = Vec::new();
    for code in Code::values() {
        if code_gen(code).is_none() {
            continue;
        }
        if !code.cpuid_features().contains(&target) {
            continue;
        }
        match probe_code(code) {
            Some(Probe::Lifted) => lifted += 1,
            Some(Probe::Unsupported) => missing.push(format!("{code:?}")),
            _ => {}
        }
    }
    missing.sort();
    (lifted, missing)
}

/// Path to the checked-in CPUID waiver file (features advertised but not yet fully
/// lifted, each with a reason).
pub fn cpuid_waiver_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("compat")
        .join("cpuid-waivers.ron")
}

/// Load the waiver set: feature names that may be advertised despite partial lift
/// coverage. Panics on a malformed file (a broken waiver list must not silently
/// pass the consistency test).
pub fn cpuid_waivers() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(cpuid_waiver_path()).expect("read cpuid-waivers.ron");
    ron::from_str(&text).expect("parse cpuid-waivers.ron")
}
