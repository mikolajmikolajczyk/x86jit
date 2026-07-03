//! State comparator (testing.md §5). Pinpoints exactly what diverged — a test
//! that only says "states differ" is useless for debugging. Undefined flags are
//! masked per-vector so differential runs don't chase architecturally-undefined
//! bits (§5 note).

use std::fmt;

use crate::oracle::RunOutcome;
use crate::vector::{ExitKind, FlagName, MemChunk, TestVector};

const REG_NAMES: [&str; 16] = [
    "RAX", "RCX", "RDX", "RBX", "RSP", "RBP", "RSI", "RDI", "R8", "R9", "R10", "R11", "R12", "R13",
    "R14", "R15",
];

#[derive(Debug, Default, PartialEq)]
pub struct Divergence {
    pub reg_diffs: Vec<(String, u64, u64)>,
    pub flag_diffs: Vec<(FlagName, bool, bool)>,
    pub mem_diffs: Vec<(u64, u8, u8)>,
    pub exit_diff: Option<(ExitKind, ExitKind)>,
}

impl Divergence {
    fn is_empty(&self) -> bool {
        self.reg_diffs.is_empty()
            && self.flag_diffs.is_empty()
            && self.mem_diffs.is_empty()
            && self.exit_diff.is_none()
    }
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, exp, got) in &self.reg_diffs {
            writeln!(f, "  reg {name}: expected {exp:#018x}  got {got:#018x}")?;
        }
        for (flag, exp, got) in &self.flag_diffs {
            writeln!(f, "  flag {flag:?}: expected {exp}  got {got}")?;
        }
        for (addr, exp, got) in &self.mem_diffs {
            writeln!(f, "  mem {addr:#x}: expected {exp:#04x}  got {got:#04x}")?;
        }
        if let Some((exp, got)) = &self.exit_diff {
            writeln!(f, "  exit: expected {exp:?}  got {got:?}")?;
        }
        Ok(())
    }
}

/// Compare two outcomes. `None` = match; `Some(d)` = the exact differences.
/// Flags named in `dont_care` are ignored (undefined-flag masking).
pub fn compare(
    expected: &RunOutcome,
    got: &RunOutcome,
    dont_care: &[FlagName],
) -> Option<Divergence> {
    let mut d = Divergence::default();

    for (i, name) in REG_NAMES.iter().enumerate() {
        if expected.cpu.gpr[i] != got.cpu.gpr[i] {
            d.reg_diffs
                .push((name.to_string(), expected.cpu.gpr[i], got.cpu.gpr[i]));
        }
    }
    if expected.cpu.rip != got.cpu.rip {
        d.reg_diffs
            .push(("RIP".into(), expected.cpu.rip, got.cpu.rip));
    }
    if expected.cpu.fs_base != got.cpu.fs_base {
        d.reg_diffs
            .push(("FS_BASE".into(), expected.cpu.fs_base, got.cpu.fs_base));
    }
    if expected.cpu.gs_base != got.cpu.gs_base {
        d.reg_diffs
            .push(("GS_BASE".into(), expected.cpu.gs_base, got.cpu.gs_base));
    }

    let (ef, gf) = (&expected.cpu.flags, &got.cpu.flags);
    for (flag, exp, got_v) in [
        (FlagName::Cf, ef.cf, gf.cf),
        (FlagName::Pf, ef.pf, gf.pf),
        (FlagName::Af, ef.af, gf.af),
        (FlagName::Zf, ef.zf, gf.zf),
        (FlagName::Sf, ef.sf, gf.sf),
        (FlagName::Of, ef.of, gf.of),
        (FlagName::Df, ef.df, gf.df),
    ] {
        if exp != got_v && !dont_care.contains(&flag) {
            d.flag_diffs.push((flag, exp, got_v));
        }
    }

    for exp_chunk in &expected.mem {
        if let Some(got_chunk) = got.mem.iter().find(|c| c.addr == exp_chunk.addr) {
            for (off, (&e, &g)) in exp_chunk.bytes.iter().zip(&got_chunk.bytes).enumerate() {
                if e != g {
                    d.mem_diffs.push((exp_chunk.addr + off as u64, e, g));
                }
            }
        }
    }

    if expected.exit != got.exit {
        d.exit_diff = Some((expected.exit, got.exit));
    }

    if d.is_empty() {
        None
    } else {
        Some(d)
    }
}

/// Check an engine outcome against a stored vector's expectation. Reconstructs the
/// expected memory image from `mem_init` overlaid with `expect.mem_diff` (a
/// changed region's FINAL bytes, keyed by the same address as a `mem_init` chunk),
/// then defers to [`compare`].
pub fn check(vector: &TestVector, got: &RunOutcome) -> Option<Divergence> {
    let expected_mem: Vec<MemChunk> = vector
        .mem_init
        .iter()
        .map(|init| {
            vector
                .expect
                .mem_diff
                .iter()
                .find(|c| c.addr == init.addr)
                .cloned()
                .unwrap_or_else(|| init.clone())
        })
        .collect();

    let expected = RunOutcome {
        cpu: vector.expect.cpu.clone(),
        mem: expected_mem,
        exit: vector.expect.exit,
    };
    compare(&expected, got, &vector.dont_care_flags)
}
