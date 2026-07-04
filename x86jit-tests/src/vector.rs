//! The test vector — the central artifact (testing.md §2). A self-contained
//! package (initial state + code + expected final state) that runs
//! deterministically without an oracle once generated. Serialized as RON with
//! byte blobs as hex strings (testing.md §3).
//!
//! These types are harness-local and serde-derived on purpose: `x86jit-core`
//! stays dependency-light (iced only), so we mirror its `Flags`/`RegionKind`
//! here and convert at the oracle boundary.

use serde::{Deserialize, Serialize};
use x86jit_core::{Flags, RegionKind};

/// Flags snapshot (mirror of `x86jit_core::Flags`, §3.2).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnapFlags {
    pub cf: bool,
    pub pf: bool,
    pub af: bool,
    pub zf: bool,
    pub sf: bool,
    pub of: bool,
    pub df: bool,
}

impl From<Flags> for SnapFlags {
    fn from(f: Flags) -> Self {
        Self {
            cf: f.cf,
            pf: f.pf,
            af: f.af,
            zf: f.zf,
            sf: f.sf,
            of: f.of,
            df: f.df,
        }
    }
}

impl From<SnapFlags> for Flags {
    fn from(f: SnapFlags) -> Self {
        Flags {
            cf: f.cf,
            pf: f.pf,
            af: f.af,
            zf: f.zf,
            sf: f.sf,
            of: f.of,
            df: f.df,
        }
    }
}

/// Named flag, for `dont_care_flags` masking (testing.md §5) and divergence reports.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlagName {
    Cf,
    Pf,
    Af,
    Zf,
    Sf,
    Of,
    Df,
}

/// Full CPU snapshot: GPRs (x86 encoding order) + rip + flags + segment bases +
/// XMM vector registers.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CpuSnapshot {
    pub gpr: [u64; 16],
    pub rip: u64,
    pub flags: SnapFlags,
    pub fs_base: u64,
    pub gs_base: u64,
    #[serde(default, with = "xmm_hex")]
    pub xmm: [u128; 16],
}

/// serde helper: `[u128; 16]` <-> array of 32-hex-digit strings (readable, and
/// avoids RON's shaky u128 support).
mod xmm_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(xmm: &[u128; 16], s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(16))?;
        for v in xmm {
            seq.serialize_element(&format!("{v:032x}"))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u128; 16], D::Error> {
        let strs = <Vec<String>>::deserialize(d)?;
        let mut out = [0u128; 16];
        for (o, s) in out.iter_mut().zip(&strs) {
            *o = u128::from_str_radix(s, 16).map_err(serde::de::Error::custom)?;
        }
        Ok(out)
    }
}

/// Region behaviour (mirror of `x86jit_core::RegionKind`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemKind {
    Ram,
    Trap,
}

impl From<MemKind> for RegionKind {
    fn from(k: MemKind) -> Self {
        match k {
            MemKind::Ram => RegionKind::Ram,
            MemKind::Trap => RegionKind::Trap,
        }
    }
}

/// A contiguous chunk of guest memory (code or data). Bytes serialize as hex.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct MemChunk {
    pub addr: u64,
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
    pub kind: MemKind,
}

/// How much to execute (testing.md §2).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunSpec {
    /// Execute exactly N blocks, then compare.
    Blocks(u64),
    /// Execute until the engine returns an Exit (e.g. a terminating `hlt`).
    UntilExit,
}

/// Direction of a faulting access, mirror of `x86jit_core::AccessKind`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}

/// How execution ended — the serializable projection of `x86jit_core::Exit`
/// plus `Budget` for a `RunSpec::Blocks(N)` run that completed without a trap.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    Hlt,
    Syscall,
    UnmappedMemory { addr: u64, access: Access },
    MmioRead { addr: u64, size: u8 },
    MmioWrite { addr: u64, size: u8, value: u64 },
    UnknownInstruction { addr: u64 },
    Exception { addr: u64, vector: u8 },
    Budget,
}

/// Expected final state (testing.md §2). `mem_diff` records ONLY the regions that
/// changed, so the comparator can assert exactly those and only those changed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Expectation {
    pub cpu: CpuSnapshot,
    pub mem_diff: Vec<MemChunk>,
    pub exit: ExitKind,
}

/// The vector itself.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TestVector {
    pub name: String,
    pub note: String,
    pub tags: Vec<String>,

    pub cpu_init: CpuSnapshot,
    pub mem_init: Vec<MemChunk>,
    pub entry: u64,
    pub run: RunSpec,

    pub expect: Expectation,

    /// Architecturally-undefined flags to ignore when comparing (testing.md §5).
    #[serde(default)]
    pub dont_care_flags: Vec<FlagName>,
}

impl TestVector {
    /// Pretty RON, ready to write to a `.ron` file.
    pub fn to_ron(&self) -> String {
        let cfg = ron::ser::PrettyConfig::new().struct_names(true);
        ron::ser::to_string_pretty(self, cfg).expect("vector serializes")
    }

    pub fn from_ron(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(text)
    }
}

/// serde helper: `Vec<u8>` <-> lowercase hex string.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TestVector {
        TestVector {
            name: "add_r32_zeroes_upper".into(),
            note: "writing eax zeroes the upper 32 bits of rax".into(),
            tags: vec!["flags".into(), "zero-extend".into()],
            cpu_init: CpuSnapshot {
                rip: 0x1000,
                ..Default::default()
            },
            mem_init: vec![MemChunk {
                addr: 0x1000,
                bytes: vec![0x01, 0xd8, 0xf4],
                kind: MemKind::Ram,
            }],
            entry: 0x1000,
            run: RunSpec::UntilExit,
            expect: Expectation {
                cpu: CpuSnapshot {
                    rip: 0x1003,
                    ..Default::default()
                },
                mem_diff: vec![],
                exit: ExitKind::Hlt,
            },
            dont_care_flags: vec![FlagName::Af],
        }
    }

    #[test]
    fn ron_roundtrip() {
        let v = sample();
        let text = v.to_ron();
        assert!(text.contains("01d8f4"), "bytes serialize as hex: {text}");
        let back = TestVector::from_ron(&text).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn flags_convert_both_ways() {
        let f = SnapFlags {
            cf: true,
            zf: true,
            ..Default::default()
        };
        let core: Flags = f.into();
        assert!(core.cf && core.zf && !core.of);
        assert_eq!(SnapFlags::from(core), f);
    }
}
