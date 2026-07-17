//! TomHarte / SingleStepTests **8088** per-instruction corpus loader — a
//! silicon-derived real-mode CPU oracle for `CpuMode::Real16`.
//!
//! The corpus (by Daniel Balsom & Folkert van Heusden, captured from a real 8088
//! running in maximum mode with an attached 8288 bus controller) ships one gzipped
//! JSON file per opcode, each holding 10 000 tests. A test is a full initial
//! CPU+RAM state and the expected final CPU+RAM state (plus a per-cycle bus trace we
//! ignore). We replay each on the x86jit **interpreter** in Real16 — one instruction
//! per test via [`x86jit_core::Vcpu::step_instruction`] — and compare the
//! architecturally defined final state.
//!
//! This is a stronger real-mode oracle than the Unicorn `MODE_16` differential
//! (`cf16.rs`): the truth is real hardware, not another emulator.
//!
//! # Corpus fetch
//! The JSON is large (~800 MB) and gitignored, not vendored. `vendor/8088/fetch.sh`
//! pulls it from `SingleStepTests/ProcessorTests` into `vendor/8088/v1/`. The loader
//! decompresses each `.json.gz` on load. When the directory is absent the test
//! **skips with a clear message** (CI must fetch first).
//!
//! # Flag masking
//! The 8088 leaves some flags **undefined** after certain ops (AF/OF/PF for BCD,
//! some shifts, muls, …). The corpus's `8088.json` metadata gives a per-opcode
//! (and per-modrm-`reg`) 16-bit `flags-mask`: ANDing it clears the bits the CPU
//! leaves undefined. We apply the *same* mask to both the expected and the actual
//! FLAGS word before comparing, so only architecturally defined flag bits are
//! checked (see [`OpcodeMeta::flags_mask`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use x86jit_core::lift::CpuMode;
use x86jit_core::{Exit, MemoryModel, Prot, RegionKind, StepResult, Vm, VmConfig};

/// The real 8088 addresses a full 1 MB of RAM; every corpus test assumes it is all
/// mapped and writable. Page-aligned (0x100000).
const GUEST_RAM: u64 = 0x10_0000;

/// FLAGS bit positions (8086/8088 layout) used to project the corpus's 16-bit FLAGS
/// word onto — and back off of — the interpreter. CF=0, PF=2, AF=4, ZF=6, SF=7,
/// TF=8, IF=9, DF=10, OF=11. We model exactly the bits `Flags::{to,set}_flags16`
/// do; the others (TF/IOPL/NT and reserved) are not architecturally defined state
/// our real-mode model tracks and are masked out of the comparison unconditionally
/// (see [`DEFINED_FLAGS`]).
///
/// Bits the interpreter models and the corpus defines: CF/PF/AF/ZF/SF/IF/DF/OF plus
/// the always-1 reserved bit 1. Everything else is excluded from the compare.
const DEFINED_FLAGS: u16 = (1 << 0) // CF
    | (1 << 2)  // PF
    | (1 << 4)  // AF
    | (1 << 6)  // ZF
    | (1 << 7)  // SF
    | (1 << 9)  // IF
    | (1 << 10) // DF
    | (1 << 11); // OF

// ---------------------------------------------------------------------------
// JSON shapes
// ---------------------------------------------------------------------------

/// One corpus test (`8088/v1/XX.json.gz` is a JSON array of these). `cycles` and
/// `test_hash` are present in the file but not deserialized — we validate final
/// state only, not the bus trace.
#[derive(Deserialize)]
pub struct HarteTest {
    pub name: String,
    /// The full instruction bytes (convenience field; we execute from `initial.ram`).
    pub bytes: Vec<u8>,
    pub initial: HarteState,
    #[serde(rename = "final")]
    pub final_: HarteState,
}

/// A CPU + RAM snapshot. `ram` is a list of `[address, byte]` pairs at **physical**
/// (linear) addresses in the 1 MB space.
#[derive(Deserialize)]
pub struct HarteState {
    pub regs: HarteRegs,
    #[serde(default)]
    pub ram: Vec<[i64; 2]>,
}

/// The register file. All are 16-bit values (0..=65535). The `final` block may omit
/// registers that did not change, so every field defaults to "absent" and the runner
/// falls back to the initial value.
#[derive(Deserialize, Default, Clone, Copy)]
pub struct HarteRegs {
    pub ax: Option<u16>,
    pub bx: Option<u16>,
    pub cx: Option<u16>,
    pub dx: Option<u16>,
    pub cs: Option<u16>,
    pub ss: Option<u16>,
    pub ds: Option<u16>,
    pub es: Option<u16>,
    pub sp: Option<u16>,
    pub bp: Option<u16>,
    pub si: Option<u16>,
    pub di: Option<u16>,
    pub ip: Option<u16>,
    pub flags: Option<u16>,
}

// ---------------------------------------------------------------------------
// Opcode metadata (`8088.json`) — undefined-flag masks
// ---------------------------------------------------------------------------

/// A node in `8088.json`: either a leaf opcode entry, or a group with a `reg`
/// subtable keyed by the modrm reg field (`"0".."7"`).
#[derive(Deserialize, Default, Clone)]
pub struct OpcodeMeta {
    #[serde(default)]
    pub status: Option<String>,
    /// 16-bit AND mask that clears the flags the CPU leaves undefined after this op.
    /// Absent ⇒ no undefined flags (mask = 0xFFFF).
    #[serde(rename = "flags-mask", default)]
    pub flags_mask: Option<u16>,
    /// modrm-`reg` subtable (opcode groups 0x80/0x81/0x83/0xD0-0xD3/0xF6/0xF7/0xFE/0xFF).
    #[serde(default)]
    pub reg: Option<BTreeMap<String, OpcodeMeta>>,
}

impl OpcodeMeta {
    /// The undefined-flag AND mask for this leaf (0xFFFF when none). Undefined flag
    /// bits are those the mask clears; we drop them from both sides of the compare.
    pub fn mask(&self) -> u16 {
        self.flags_mask.unwrap_or(0xFFFF)
    }
}

/// The whole `8088.json` metadata table, opcode-hex-string keyed.
pub struct Metadata(BTreeMap<String, OpcodeMeta>);

impl Metadata {
    /// Resolve the undefined-flag mask for `opcode`, descending into the `reg`
    /// subtable by the modrm `reg` field (bits 5:3 of the modrm byte) when the
    /// opcode is a group. `reg` is `None` for a plain opcode.
    pub fn flags_mask(&self, opcode: u8, reg: Option<u8>) -> u16 {
        let key = format!("{opcode:02X}");
        let Some(entry) = self.0.get(&key) else {
            return 0xFFFF;
        };
        if let (Some(sub), Some(r)) = (&entry.reg, reg) {
            if let Some(leaf) = sub.get(&r.to_string()) {
                return leaf.mask();
            }
        }
        entry.mask()
    }
}

// ---------------------------------------------------------------------------
// Corpus discovery / loading
// ---------------------------------------------------------------------------

/// Root of the fetched corpus (`vendor/8088/v1`), or `None` when it has not been
/// fetched. Resolved relative to this crate's manifest dir so it works from any cwd.
pub fn corpus_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/8088/v1");
    dir.is_dir().then_some(dir)
}

/// Load the `8088.json` opcode metadata from the corpus dir.
pub fn load_metadata(dir: &Path) -> Metadata {
    let text = std::fs::read_to_string(dir.join("8088.json")).expect("8088.json present in corpus");
    Metadata(serde_json::from_str(&text).expect("8088.json parses"))
}

/// The per-opcode test files present in the corpus dir, sorted. Each entry is
/// `(file_stem, path)` where `file_stem` is e.g. `"00"` or `"D0.4"` — the leading
/// two hex digits are the opcode; a `.R` suffix is the modrm-reg group member.
pub fn opcode_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".json.gz") {
            out.push((stem.to_string(), entry.path()));
        }
    }
    out.sort();
    out
}

/// Parse the opcode byte and (optional) modrm-`reg` group member from a file stem
/// like `"00"` or `"D0.4"`.
pub fn parse_stem(stem: &str) -> Option<(u8, Option<u8>)> {
    let (hex, reg) = match stem.split_once('.') {
        Some((h, r)) => (h, r.parse::<u8>().ok()),
        None => (stem, None),
    };
    let opcode = u8::from_str_radix(hex, 16).ok()?;
    Some((opcode, reg))
}

/// Decompress and parse an opcode file, taking **at most** `limit` tests (0 = all).
///
/// Each file is a JSON array of 10 000 tests, and every test carries a large per-cycle
/// bus trace (`cycles`) we do not model — deserializing a whole 15 MB file in a debug
/// build is the dominant cost of the sweep. So we stream the top-level array element by
/// element and stop after `limit`, never tokenizing the tail. `HarteTest`'s missing
/// `cycles`/`test_hash` fields are skipped per element (serde ignores unknown keys).
pub fn load_tests(path: &Path, limit: usize) -> Vec<HarteTest> {
    use std::cell::{Cell, RefCell};

    use serde::de::{DeserializeSeed, SeqAccess, Visitor};

    /// Visitor that pushes at most `limit` elements (0 = all) into `sink`, then STOPS
    /// pulling from the sequence — serde_json otherwise insists on consuming the whole
    /// (15 MB, 10 000-element) array, and every element carries a large per-cycle bus
    /// trace we don't model, so a full parse dominates the sweep. On hitting the cap it
    /// sets `capped` and returns an error to unwind; the reader is then dropped, so the
    /// unread tail is never decompressed.
    struct Capped<'a> {
        limit: usize,
        sink: &'a RefCell<Vec<HarteTest>>,
        capped: &'a Cell<bool>,
    }

    impl<'de> Visitor<'de> for Capped<'_> {
        type Value = ();

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an array of 8088 corpus tests")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut sink = self.sink.borrow_mut();
            while self.limit == 0 || sink.len() < self.limit {
                match seq.next_element::<HarteTest>()? {
                    Some(t) => sink.push(t),
                    None => return Ok(()), // consumed the whole (short) array cleanly
                }
            }
            // Cap reached with elements still unread: unwind rather than draining. The
            // error serde_json surfaces to the caller is its own (it expected the array
            // to close), so we flag `capped` here to tell a deliberate stop from a real
            // parse failure.
            self.capped.set(true);
            Err(serde::de::Error::custom("capped"))
        }
    }

    impl<'de> DeserializeSeed<'de> for Capped<'_> {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_seq(self)
        }
    }

    let file = std::fs::File::open(path).expect("opcode file opens");
    let gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut de = serde_json::Deserializer::from_reader(gz);
    let sink = RefCell::new(Vec::new());
    let capped = Cell::new(false);
    let seed = Capped {
        limit,
        sink: &sink,
        capped: &capped,
    };
    match seed.deserialize(&mut de) {
        Ok(()) => {}
        // We stopped on purpose once the cap was hit; otherwise it's a real parse error.
        Err(_) if capped.get() => {}
        Err(e) => panic!("opcode json parses: {e}"),
    }
    sink.into_inner()
}

// ---------------------------------------------------------------------------
// The oracle: replay one test on the Real16 interpreter
// ---------------------------------------------------------------------------

/// Outcome of running a single corpus test.
#[derive(Debug, PartialEq, Eq)]
pub enum TestOutcome {
    /// Final CPU + RAM state matched (after undefined-flag masking).
    Pass,
    /// The interpreter executed the instruction but reached a different final state.
    /// Carries a short human diff for the first divergence.
    Fail(String),
    /// The Real16 lifter does not yet support this opcode (`Exit::UnknownInstruction`).
    /// Counted and listed per-opcode, never silently passed.
    Unsupported,
    /// The test exercises the 8088's **20-bit address-bus wraparound**: a segment:offset
    /// whose linear address `(sel<<4)+off` reaches ≥ 1 MB wraps to `& 0xFFFFF` on real
    /// hardware, and the corpus stores RAM at the wrapped address. The Real16 interpreter
    /// computes `cs_base + offset` WITHOUT the 20-bit wrap (`lift/mod.rs::real16`), so it
    /// fetches/accesses past the 1 MB map and traps `UnmappedMemory` at addr ≥ 0x100000.
    /// Counted as a **distinct, documented gap** (not a generic trap or a wrong answer):
    /// modeling the wrap is follow-up interpreter work, not a loader problem.
    AddrWrap,
    /// The interpreter trapped out in a way that is not a plain "unsupported opcode" or
    /// the address-wrap gap (unmapped memory, port I/O, a CPU exception the corpus did
    /// not lead into, …). Kept distinct from `Fail` so genuine surprises stand out.
    Trapped(String),
}

/// Top of the 8088 physical address space (20-bit bus). Segment:offset arithmetic
/// wraps here on real hardware.
const ADDR_SPACE: u64 = 0x10_0000;

/// A reusable Real16 execution harness that owns one 1 MB flat `Vm` across many
/// corpus tests. Allocating a fresh 1 MB buffer per test (10 000 × 325 opcodes)
/// dominates the run, so [`Runner::run`] instead re-seeds only the RAM each test
/// touches and, afterwards, zeroes exactly the union of the addresses that test
/// read or wrote — leaving the buffer clean for the next test without a re-allocation
/// or a full 1 MB memset. Correct because the corpus lists **every** address an
/// instruction reads in `initial.ram` and every address it writes in `final.ram`.
pub struct Runner {
    vm: Vm,
    /// Addresses to zero after a test — its `initial.ram ∪ final.ram`, reused as
    /// scratch each call.
    touched: Vec<u64>,
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    /// Build the reusable Real16 VM with a full 1 MB of flat RAM mapped RWX.
    pub fn new() -> Self {
        let mut vm = Vm::with_backend(
            VmConfig {
                memory_model: MemoryModel::Flat { size: GUEST_RAM },
                consistency: x86jit_core::MemConsistency::Fast,
            },
            Box::new(x86jit_core::InterpreterBackend),
        );
        vm.set_cpu_mode(CpuMode::Real16);
        vm.map(0, GUEST_RAM as usize, Prot::RWX, RegionKind::Ram)
            .expect("1 MB flat maps");
        Runner {
            vm,
            touched: Vec::new(),
        }
    }

    /// Replay one corpus test on the interpreter in `CpuMode::Real16` and classify
    /// the outcome. `flags_mask` is the opcode's undefined-flag AND mask from the
    /// metadata.
    ///
    /// The 8088 has no notion of an invalid instruction — for opcodes whose final
    /// state the corpus captures, a divide fault (`F6.6/.7`, `F7.6/.7`, `D4`) vectors
    /// through the IVT (INT0 → 0x0400) into a handler that is a wall of `0x90` NOPs in
    /// the seeded RAM. We run exactly one instruction, so on a `#DE` the interpreter's
    /// in-guest vectoring runs and the final IP/CS/flags land on the handler entry —
    /// which is what the corpus records.
    pub fn run(&mut self, test: &HarteTest, flags_mask: u16) -> TestOutcome {
        self.touched.clear();
        for &[addr, byte] in &test.initial.ram {
            self.vm
                .write_bytes(addr as u64, &[byte as u8])
                .expect("ram address within 1 MB");
            self.touched.push(addr as u64);
        }

        let mut cpu = self.vm.new_vcpu();
        load_regs(&mut cpu.cpu, &test.initial.regs);

        // The linear fetch address on real hardware. If it (or any operand EA) reaches
        // ≥ 1 MB the 8088 wraps; our interpreter does not, so such a test is the
        // documented address-wrap gap rather than a generic trap (see `AddrWrap`).
        let init = &test.initial.regs;
        let fetch_lin = ((init.cs.unwrap_or(0) as u64) << 4) + init.ip.unwrap_or(0) as u64;

        // Execute exactly one instruction. `step_instruction` delivers real-mode `#DE`
        // (and lifted `int`/`ud2`) exceptions in-guest through the IVT — so a divide
        // fault returns `Continue` with IP/CS on the handler, as the corpus records.
        let outcome = match cpu.step_instruction(&self.vm) {
            StepResult::Continue => self.compare(test, flags_mask, &cpu),
            StepResult::Exit(Exit::UnknownInstruction { .. }) => TestOutcome::Unsupported,
            // A fetch/access past the 1 MB map is the unmodeled 20-bit wraparound.
            StepResult::Exit(Exit::UnmappedMemory { addr, .. })
                if addr >= ADDR_SPACE || fetch_lin >= ADDR_SPACE =>
            {
                TestOutcome::AddrWrap
            }
            // Not an unsupported opcode or the wrap gap: a genuine trap (port I/O, an
            // in-bounds unmapped access, …) — kept distinct from a wrong-answer `Fail`.
            StepResult::Exit(other) => TestOutcome::Trapped(format!("{other:?}")),
        };

        // Restore the buffer to all-zero over exactly what this test touched, so the
        // next test starts from a clean slate without re-allocating 1 MB.
        for &[addr, _] in &test.final_.ram {
            self.touched.push(addr as u64);
        }
        for &addr in &self.touched {
            self.vm.write_bytes(addr, &[0u8]).expect("addr mapped");
        }
        outcome
    }

    /// Diff the architecturally defined final CPU + RAM state against the corpus's
    /// `final` block, applying the undefined-flag mask.
    fn compare(&self, test: &HarteTest, flags_mask: u16, cpu: &x86jit_core::Vcpu) -> TestOutcome {
        let mut diffs: Vec<String> = Vec::new();
        let exp = &test.final_.regs;
        let init = &test.initial.regs;
        let cmp =
            |name: &str, exp: Option<u16>, init: Option<u16>, got: u16, diffs: &mut Vec<String>| {
                // A `final` register absent from the JSON kept its initial value.
                let want = exp.or(init).unwrap_or(0);
                if want != got {
                    diffs.push(format!("{name}: want {want:#06x} got {got:#06x}"));
                }
            };
        cmp("ax", exp.ax, init.ax, cpu.cpu.gpr[0] as u16, &mut diffs);
        cmp("cx", exp.cx, init.cx, cpu.cpu.gpr[1] as u16, &mut diffs);
        cmp("dx", exp.dx, init.dx, cpu.cpu.gpr[2] as u16, &mut diffs);
        cmp("bx", exp.bx, init.bx, cpu.cpu.gpr[3] as u16, &mut diffs);
        cmp("sp", exp.sp, init.sp, cpu.cpu.gpr[4] as u16, &mut diffs);
        cmp("bp", exp.bp, init.bp, cpu.cpu.gpr[5] as u16, &mut diffs);
        cmp("si", exp.si, init.si, cpu.cpu.gpr[6] as u16, &mut diffs);
        cmp("di", exp.di, init.di, cpu.cpu.gpr[7] as u16, &mut diffs);
        cmp("cs", exp.cs, init.cs, cpu.cpu.cs, &mut diffs);
        cmp("ss", exp.ss, init.ss, cpu.cpu.ss, &mut diffs);
        cmp("ds", exp.ds, init.ds, cpu.cpu.ds, &mut diffs);
        cmp("es", exp.es, init.es, cpu.cpu.es, &mut diffs);
        cmp("ip", exp.ip, init.ip, cpu.cpu.rip as u16, &mut diffs);

        // Flags: mask off (a) bits the interpreter/corpus don't both model and (b) the
        // bits this opcode leaves undefined, then compare only what's left.
        let mask = DEFINED_FLAGS & flags_mask;
        let want_flags = exp.flags.or(init.flags).unwrap_or(0) & mask;
        let got_flags = cpu.cpu.flags.to_flags16() & mask;
        if want_flags != got_flags {
            diffs.push(format!(
                "flags: want {want_flags:#06x} got {got_flags:#06x} (mask {mask:#06x})"
            ));
        }

        // Memory: every byte the `final.ram` lists must read back to that value.
        for &[addr, byte] in &test.final_.ram {
            let mut buf = [0u8];
            self.vm
                .read_bytes(addr as u64, &mut buf)
                .expect("final ram address mapped");
            if buf[0] != byte as u8 {
                diffs.push(format!(
                    "mem[{addr:#07x}]: want {:#04x} got {:#04x}",
                    byte as u8, buf[0]
                ));
            }
        }

        if diffs.is_empty() {
            TestOutcome::Pass
        } else {
            TestOutcome::Fail(diffs.join("; "))
        }
    }
}

/// Convenience one-shot: replay a single test on a fresh [`Runner`]. Used by the
/// hand-written fixtures; the corpus sweep reuses one `Runner` across all tests.
pub fn run_test(test: &HarteTest, flags_mask: u16) -> TestOutcome {
    Runner::new().run(test, flags_mask)
}

/// Seed the interpreter's `CpuState` from a corpus register block. Absent registers
/// (only possible in a `final` block; every `initial` block is complete) leave the
/// field at 0.
fn load_regs(cpu: &mut x86jit_core::state::CpuState, r: &HarteRegs) {
    // GPRs in x86 encoding order: AX=0, CX=1, DX=2, BX=3, SP=4, BP=5, SI=6, DI=7.
    cpu.gpr[0] = r.ax.unwrap_or(0) as u64;
    cpu.gpr[1] = r.cx.unwrap_or(0) as u64;
    cpu.gpr[2] = r.dx.unwrap_or(0) as u64;
    cpu.gpr[3] = r.bx.unwrap_or(0) as u64;
    cpu.gpr[4] = r.sp.unwrap_or(0) as u64;
    cpu.gpr[5] = r.bp.unwrap_or(0) as u64;
    cpu.gpr[6] = r.si.unwrap_or(0) as u64;
    cpu.gpr[7] = r.di.unwrap_or(0) as u64;
    cpu.cs = r.cs.unwrap_or(0);
    cpu.ss = r.ss.unwrap_or(0);
    cpu.ds = r.ds.unwrap_or(0);
    cpu.es = r.es.unwrap_or(0);
    cpu.rip = r.ip.unwrap_or(0) as u64;
    cpu.flags.set_flags16(r.flags.unwrap_or(0));
}

// ---------------------------------------------------------------------------
// Aggregate tally
// ---------------------------------------------------------------------------

/// Per-opcode outcome counts, plus a sample failure message.
#[derive(Default)]
pub struct OpTally {
    pub pass: u64,
    pub fail: u64,
    pub unsupported: u64,
    /// Tests skipped for the unmodeled 20-bit address wraparound (see [`TestOutcome::AddrWrap`]).
    pub addr_wrap: u64,
    pub trapped: u64,
    /// First failing/trapped test's name + diff, for the gap report.
    pub sample: Option<String>,
}

impl OpTally {
    pub fn total(&self) -> u64 {
        self.pass + self.fail + self.unsupported + self.addr_wrap + self.trapped
    }
}

/// The whole run's tally, keyed by opcode file stem (`"00"`, `"D0.4"`, …).
#[derive(Default)]
pub struct Summary {
    pub by_op: BTreeMap<String, OpTally>,
}

impl Summary {
    pub fn record(&mut self, stem: &str, test_name: &str, outcome: TestOutcome) {
        let t = self.by_op.entry(stem.to_string()).or_default();
        match outcome {
            TestOutcome::Pass => t.pass += 1,
            TestOutcome::Unsupported => t.unsupported += 1,
            TestOutcome::AddrWrap => t.addr_wrap += 1,
            TestOutcome::Fail(d) => {
                t.fail += 1;
                if t.sample.is_none() {
                    t.sample = Some(format!("`{test_name}`: {d}"));
                }
            }
            TestOutcome::Trapped(d) => {
                t.trapped += 1;
                if t.sample.is_none() {
                    t.sample = Some(format!("`{test_name}` TRAP: {d}"));
                }
            }
        }
    }

    pub fn totals(&self) -> OpTally {
        let mut acc = OpTally::default();
        for t in self.by_op.values() {
            acc.pass += t.pass;
            acc.fail += t.fail;
            acc.unsupported += t.unsupported;
            acc.addr_wrap += t.addr_wrap;
            acc.trapped += t.trapped;
        }
        acc
    }
}
