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
    pub xmm_diffs: Vec<(usize, u128, u128)>,
    pub ymm_hi_diffs: Vec<(usize, u128, u128)>,
    /// `(reg, half, expected, got)` — half 0 = bits 383:256, half 1 = bits 511:384.
    pub zmm_hi_diffs: Vec<(usize, usize, u128, u128)>,
    pub kmask_diffs: Vec<(usize, u64, u64)>,
    /// x87 register-stack diffs: `(ST index, expected 80-bit bytes, got 80-bit bytes)`.
    pub st_diffs: Vec<(usize, [u8; 10], [u8; 10])>,
    /// x87 control-word diff `(expected, got)`.
    pub fpu_cw_diff: Option<(u16, u16)>,
    /// x87 status-word TOP-field diff `(expected, got)`.
    pub fpu_top_diff: Option<(u8, u8)>,
    pub flag_diffs: Vec<(FlagName, bool, bool)>,
    pub mem_diffs: Vec<(u64, u8, u8)>,
    /// Expected memory chunks the actual result did not carry: `(addr, expected len)`.
    pub missing_mem: Vec<(u64, usize)>,
    /// Chunks present in the actual result but not expected: `(addr, len)`.
    pub extra_mem: Vec<(u64, usize)>,
    /// A chunk present on both sides at different lengths: `(addr, expected, got)`.
    pub mem_len_diffs: Vec<(u64, usize, usize)>,
    pub exit_diff: Option<(ExitKind, ExitKind)>,
}

impl Divergence {
    fn is_empty(&self) -> bool {
        self.reg_diffs.is_empty()
            && self.xmm_diffs.is_empty()
            && self.ymm_hi_diffs.is_empty()
            && self.zmm_hi_diffs.is_empty()
            && self.kmask_diffs.is_empty()
            && self.st_diffs.is_empty()
            && self.fpu_cw_diff.is_none()
            && self.fpu_top_diff.is_none()
            && self.flag_diffs.is_empty()
            && self.mem_diffs.is_empty()
            && self.missing_mem.is_empty()
            && self.extra_mem.is_empty()
            && self.mem_len_diffs.is_empty()
            && self.exit_diff.is_none()
    }
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, exp, got) in &self.reg_diffs {
            writeln!(f, "  reg {name}: expected {exp:#018x}  got {got:#018x}")?;
        }
        for (i, exp, got) in &self.xmm_diffs {
            writeln!(f, "  xmm{i}: expected {exp:#034x}  got {got:#034x}")?;
        }
        for (i, exp, got) in &self.ymm_hi_diffs {
            writeln!(f, "  ymm{i}.hi: expected {exp:#034x}  got {got:#034x}")?;
        }
        for (i, half, exp, got) in &self.zmm_hi_diffs {
            writeln!(
                f,
                "  zmm{i}.hi[{half}]: expected {exp:#034x}  got {got:#034x}"
            )?;
        }
        for (i, exp, got) in &self.kmask_diffs {
            writeln!(f, "  k{i}: expected {exp:#018x}  got {got:#018x}")?;
        }
        for (i, exp, got) in &self.st_diffs {
            writeln!(
                f,
                "  st({i}): expected {}  got {}",
                hex::encode(exp),
                hex::encode(got)
            )?;
        }
        if let Some((exp, got)) = &self.fpu_cw_diff {
            writeln!(f, "  fpu_cw: expected {exp:#06x}  got {got:#06x}")?;
        }
        if let Some((exp, got)) = &self.fpu_top_diff {
            writeln!(f, "  fpu_top: expected {exp}  got {got}")?;
        }
        for (flag, exp, got) in &self.flag_diffs {
            writeln!(f, "  flag {flag:?}: expected {exp}  got {got}")?;
        }
        for (addr, exp, got) in &self.mem_diffs {
            writeln!(f, "  mem {addr:#x}: expected {exp:#04x}  got {got:#04x}")?;
        }
        for (addr, len) in &self.missing_mem {
            writeln!(
                f,
                "  mem {addr:#x}: expected {len} byte(s), the result has no such chunk"
            )?;
        }
        for (addr, len) in &self.extra_mem {
            writeln!(f, "  mem {addr:#x}: {len} unexpected byte(s) in the result")?;
        }
        for (addr, exp, got) in &self.mem_len_diffs {
            writeln!(f, "  mem {addr:#x}: expected {exp} byte(s)  got {got}")?;
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
    for i in 0..16 {
        if expected.cpu.xmm[i] != got.cpu.xmm[i] {
            d.xmm_diffs.push((i, expected.cpu.xmm[i], got.cpu.xmm[i]));
        }
        if expected.cpu.ymm_hi[i] != got.cpu.ymm_hi[i] {
            d.ymm_hi_diffs
                .push((i, expected.cpu.ymm_hi[i], got.cpu.ymm_hi[i]));
        }
        for half in 0..2 {
            if expected.cpu.zmm_hi[i][half] != got.cpu.zmm_hi[i][half] {
                d.zmm_hi_diffs.push((
                    i,
                    half,
                    expected.cpu.zmm_hi[i][half],
                    got.cpu.zmm_hi[i][half],
                ));
            }
        }
    }
    for i in 0..8 {
        if expected.cpu.kmask[i] != got.cpu.kmask[i] {
            d.kmask_diffs
                .push((i, expected.cpu.kmask[i], got.cpu.kmask[i]));
        }
    }

    // x87 register stack (task-132): compared in architectural ST(0..7) order on both
    // sides (the oracles de-rotate to ST order), plus the control word and the
    // status-word TOP field. The C0–C3 condition codes are intentionally NOT compared:
    // the interp derives its status word from `fpu_top` and leaves them zero (§14).
    for i in 0..8 {
        if expected.cpu.st[i] != got.cpu.st[i] {
            d.st_diffs.push((i, expected.cpu.st[i], got.cpu.st[i]));
        }
    }
    if expected.cpu.fpu_cw != got.cpu.fpu_cw {
        d.fpu_cw_diff = Some((expected.cpu.fpu_cw, got.cpu.fpu_cw));
    }
    if expected.cpu.fpu_top != got.cpu.fpu_top {
        d.fpu_top_diff = Some((expected.cpu.fpu_top, got.cpu.fpu_top));
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

    // A chunk the actual result does not carry, or carries short, is a divergence —
    // not something to skip. `find(..)` returning None used to mean "nothing to
    // compare", and `zip` stopped at the shorter side, so an oracle that returned no
    // memory at all compared equal. Every test asserting that a guest STORE landed
    // where it should rested on this path, which made that whole class of result
    // unfalsifiable. Report the absence explicitly, using a sentinel byte so the
    // difference is visible in the diff output rather than silently absent.
    for exp_chunk in &expected.mem {
        let Some(got_chunk) = got.mem.iter().find(|c| c.addr == exp_chunk.addr) else {
            d.missing_mem.push((exp_chunk.addr, exp_chunk.bytes.len()));
            continue;
        };
        if exp_chunk.bytes.len() != got_chunk.bytes.len() {
            d.mem_len_diffs
                .push((exp_chunk.addr, exp_chunk.bytes.len(), got_chunk.bytes.len()));
        }
        for (off, (&e, &g)) in exp_chunk.bytes.iter().zip(&got_chunk.bytes).enumerate() {
            if e != g {
                d.mem_diffs.push((exp_chunk.addr + off as u64, e, g));
            }
        }
    }
    for got_chunk in &got.mem {
        if !expected.mem.iter().any(|c| c.addr == got_chunk.addr) {
            d.extra_mem.push((got_chunk.addr, got_chunk.bytes.len()));
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

/// `Some(is_quiet)` if the low `width_bytes*8` bits of `bits` encode a NaN at that float
/// width (f16/f32/f64), else `None`. Quiet iff the most-significant mantissa bit is set.
fn nan_class(bits: u64, width_bytes: u32) -> Option<bool> {
    let (exp_bits, mant_bits) = match width_bytes {
        2 => (5u32, 10u32),
        4 => (8, 23),
        8 => (11, 52),
        _ => return None,
    };
    let exp = (bits >> mant_bits) & ((1 << exp_bits) - 1);
    let mant = bits & ((1u64 << mant_bits) - 1);
    if exp == (1 << exp_bits) - 1 && mant != 0 {
        Some(mant & (1 << (mant_bits - 1)) != 0)
    } else {
        None
    }
}

/// How the differing lanes of `a`/`b` relate at one float lane width.
enum LaneRel {
    /// Every differing lane is a same-class NaN pair — this width fully explains the diff.
    Explains,
    /// Some differing lane is a NaN on both sides but of *different* class (quiet vs
    /// signaling). Hardware only ever produces a QNaN, so this is a genuine divergence.
    ClassMismatch,
    /// A differing lane is not a same-class-NaN pair (e.g. one side finite) — this width
    /// doesn't explain the diff, but doesn't condemn it either.
    Other,
}

fn width_rel(a: u128, b: u128, width_bytes: u32) -> LaneRel {
    let bits = width_bytes * 8;
    let mask: u128 = (1u128 << bits) - 1;
    let mut all_explained = true;
    for l in 0..(128 / bits) {
        let la = ((a >> (l * bits)) & mask) as u64;
        let lb = ((b >> (l * bits)) & mask) as u64;
        if la == lb {
            continue;
        }
        match (nan_class(la, width_bytes), nan_class(lb, width_bytes)) {
            (Some(qa), Some(qb)) if qa == qb => {}
            (Some(_), Some(_)) => return LaneRel::ClassMismatch,
            _ => all_explained = false,
        }
    }
    if all_explained {
        LaneRel::Explains
    } else {
        LaneRel::Other
    }
}

/// True if `a` and `b` differ only in the sign/payload of same-class NaNs at one of the given
/// float lane widths (bytes; a subset of {2,4,8}). The x86 SDM leaves a computed QNaN's sign
/// and payload unspecified, so the real CPU and the softfloat interpreter legitimately disagree
/// on those bits (FMA, cvtps2ph, …). `widths` must be the element widths the program's float
/// ops actually use — trying an unrelated width would let one type's bit pattern alias another's
/// NaN encoding (e.g. an f32 ±inf sign-flip looks like an f16 NaN payload). A quiet-vs-signaling
/// class mismatch at any tried width VETOES tolerance (hardware only emits QNaNs). (task-205)
pub fn nan_payload_equiv(a: u128, b: u128, widths: &[u32]) -> bool {
    if a == b {
        return true;
    }
    let mut any_explains = false;
    for &w in widths {
        match width_rel(a, b, w) {
            LaneRel::ClassMismatch => return false,
            LaneRel::Explains => any_explains = true,
            LaneRel::Other => {}
        }
    }
    any_explains
}

/// Like [`compare`], but tolerates the unspecified NaN sign/payload: a divergence whose ONLY
/// differences are vector-register lanes that are [`nan_payload_equiv`] is treated as a match.
/// Any non-vector diff, or a vector diff that is not pure NaN-payload, still fails. For the
/// fuzz campaign's float legs (task-205) — NOT for the strict native/JIT regression tests.
pub fn compare_nan_tolerant(
    expected: &RunOutcome,
    got: &RunOutcome,
    dont_care: &[FlagName],
    fp_widths: &[u32],
) -> Option<Divergence> {
    let d = compare(expected, got, dont_care)?;
    if fp_widths.is_empty() {
        return Some(d); // no float op → nothing to tolerate, stay strict
    }
    let non_vec_clean = d.reg_diffs.is_empty()
        && d.kmask_diffs.is_empty()
        && d.st_diffs.is_empty()
        && d.fpu_cw_diff.is_none()
        && d.fpu_top_diff.is_none()
        && d.flag_diffs.is_empty()
        && d.mem_diffs.is_empty()
        && d.exit_diff.is_none();
    let vec_all_nan = d
        .xmm_diffs
        .iter()
        .all(|(_, e, g)| nan_payload_equiv(*e, *g, fp_widths))
        && d.ymm_hi_diffs
            .iter()
            .all(|(_, e, g)| nan_payload_equiv(*e, *g, fp_widths))
        && d.zmm_hi_diffs
            .iter()
            .all(|(_, _, e, g)| nan_payload_equiv(*e, *g, fp_widths));
    if non_vec_clean && vec_all_nan {
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

#[cfg(test)]
mod nan_tests {
    use super::nan_payload_equiv;

    // Build a 128-bit register from four f32 lanes (lane 0 = low).
    fn f32x4(l: [u32; 4]) -> u128 {
        (0..4).fold(0u128, |acc, i| acc | ((l[i] as u128) << (i * 32)))
    }

    #[test]
    fn same_class_nan_payload_and_sign_tolerated() {
        // f32 qNaN differing only in payload and sign — same class → equivalent.
        assert!(nan_payload_equiv(
            f32x4([0x7fc0_0001, 0, 0, 0]),
            f32x4([0xffc0_0002, 0, 0, 0]),
            &[4],
        ));
        // f16 qNaN 0x7e00 vs 0x7e01 (the cvtps2ph witness), rest equal.
        assert!(nan_payload_equiv(0x7e00, 0x7e01, &[2]));
    }

    #[test]
    fn finite_and_class_mismatches_still_fail() {
        // Finite-vs-finite (subnormal FMA class) must NOT be tolerated.
        assert!(!nan_payload_equiv(
            f32x4([0x0000_05f8, 0, 0, 0]),
            f32x4([0x0000_0678, 0, 0, 0]),
            &[4],
        ));
        // qNaN vs SNaN (different class) must fail.
        assert!(!nan_payload_equiv(
            f32x4([0x7fc0_0001, 0, 0, 0]),
            f32x4([0x7f80_0001, 0, 0, 0]),
            &[4],
        ));
        // NaN vs finite must fail.
        assert!(!nan_payload_equiv(
            f32x4([0x7fc0_0001, 0, 0, 0]),
            f32x4([0x3f80_0000, 0, 0, 0]),
            &[4],
        ));
        // Inf vs inf with opposite sign is a real value diff, not a NaN payload — fail.
        assert!(!nan_payload_equiv(
            f32x4([0x7f80_0000, 0, 0, 0]),
            f32x4([0xff80_0000, 0, 0, 0]),
            &[4],
        ));
        // The f16 reinterpretation of that f32 ±inf sign-flip must NOT be tolerated when the
        // program's float width is f32 — only width 4 is tried, so no cross-width alias.
        assert!(!nan_payload_equiv(
            f32x4([0x7f80_0000, 0, 0, 0]),
            f32x4([0xff80_0000, 0, 0, 0]),
            &[4, 8],
        ));
    }
}

#[cfg(test)]
mod mem_compare_tests {
    use super::compare;
    use crate::oracle::RunOutcome;
    use crate::vector::{CpuSnapshot, ExitKind, MemChunk, MemKind};

    /// task-310: memory the oracle never returned must be a divergence.
    ///
    /// The comparator used to look the expected chunk up with `find(..)` and skip it
    /// when absent, then `zip` the byte slices — which stops at the shorter one. An
    /// oracle returning no memory, or a short chunk, therefore compared equal, and
    /// every test asserting that a guest store landed where it should was resting on
    /// that. These three cases fail against the old comparator and pass against this
    /// one, which is the whole point of writing them down.
    #[test]
    fn absent_and_short_memory_are_divergences() {
        let chunk = |addr, bytes: &[u8]| MemChunk {
            addr,
            bytes: bytes.to_vec(),
            kind: MemKind::Ram,
        };
        let outcome = |mem: Vec<MemChunk>| RunOutcome {
            cpu: CpuSnapshot::default(),
            mem,
            exit: ExitKind::Hlt,
        };
        let expected = outcome(vec![chunk(0x1000, &[1, 2, 3, 4])]);

        let none = outcome(vec![]);
        assert!(
            compare(&expected, &none, &[]).is_some(),
            "an oracle that returned no memory must not compare equal"
        );

        let short = outcome(vec![chunk(0x1000, &[1, 2])]);
        assert!(
            compare(&expected, &short, &[]).is_some(),
            "a truncated chunk must not compare equal on its surviving prefix"
        );

        let extra = outcome(vec![chunk(0x1000, &[1, 2, 3, 4]), chunk(0x2000, &[9])]);
        assert!(
            compare(&expected, &extra, &[]).is_some(),
            "memory the run was not asked about must be reported, not ignored"
        );

        let exact = outcome(vec![chunk(0x1000, &[1, 2, 3, 4])]);
        assert!(
            compare(&expected, &exact, &[]).is_none(),
            "identical must match"
        );
    }
}
