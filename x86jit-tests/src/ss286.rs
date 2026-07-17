//! SingleStepTests **80286** per-instruction corpus loader — the *authoritative*
//! real-mode CPU oracle for [`CpuMode::Real16`].
//!
//! The corpus (by Daniel Balsom, captured from a real `Harris N80C286-12` running
//! under the ArduinoX86 CPU-to-Arduino interface) ships one file per opcode form,
//! each holding 1 000–5 000 tests. A test is a full initial CPU+RAM state and the
//! expected final CPU+RAM state (plus a per-cycle bus trace we ignore, and an
//! optional `exception` record). We replay each on the x86jit **interpreter** in
//! `Real16` — via [`x86jit_core::Vcpu::step_instruction`] — and compare the
//! architecturally defined final state.
//!
//! Unlike the 8088 corpus ([`crate::harte`]), the 80286 is *our target CPU*. There
//! is no "generation difference" excuse for a divergence: a failure here is a bug in
//! our model. So this oracle is deliberately **stricter** than the 8088 one — it
//! checks the reserved FLAGS bit, models no address-bus wraparound gap (the 286 does
//! not wrap segment:offset at 1 MB the way the 8088 does), and validates in-guest
//! exception delivery byte-for-byte.
//!
//! # The terminating-`HALT` protocol
//! Every corpus test is *two* instructions: the instruction under test, then a
//! terminating `HALT` (opcode `0xF4`). The 286 exposes no instruction-boundary
//! signal, so the capture rig detects completion by watching for the `HALT` bus
//! cycle; the final register state (notably `IP`) is therefore recorded **after**
//! the `HALT` retires. When the instruction under test is flow-control or faults,
//! the rig injects the `HALT` at the first code fetch after the jump — and
//! materialises that `HALT` byte into `initial.ram` at the destination.
//!
//! We reproduce this exactly: [`Runner::run`] single-steps until the interpreter
//! retires the `HALT` ([`Exit::Hlt`], which advances `RIP` past the `HALT` before
//! returning), then compares. Real-mode `#DE`/`int`/`ud2` deliver in-guest through
//! the IVT and return `Continue`, so a faulting instruction naturally runs into the
//! injected handler `HALT`. No `IP` fix-up is needed — the final `IP` falls out of
//! actually retiring the same `HALT` the hardware did.
//!
//! # Test format: MOO, parsed directly
//! The 286 suite is published as **MOO** — a simple chunked binary
//! (<https://github.com/dbalsom/moo>) — not JSON. We parse MOO directly in Rust
//! (see [`parse_moo`]) rather than shelling out to the repo's `moo2json.py`
//! converter. This keeps the fetch path a plain `curl` with no Python dependency,
//! reads the gzipped `.MOO.gz` straight off disk, and — because MOO is chunked — lets
//! us *skip the multi-megabyte per-cycle bus trace* (`CYCL`) by advancing past it
//! rather than materialising it, which the JSON path cannot do. See
//! `vendor/80286/fetch.sh`.
//!
//! # Flag masking
//! The 286 leaves some flags **undefined** after certain ops. `metadata.json` gives a
//! per-opcode (and per-modrm-`reg`) 16-bit `flags-mask`: ANDing it clears the bits the
//! CPU leaves undefined (identical convention to the 8088 `8088.json`). We combine it
//! with [`DEFINED_FLAGS`] — the bits our [`Flags::to_flags16`] actually models — and
//! compare only what survives, on both the final FLAGS register and the FLAGS word an
//! exception pushes to the stack.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use x86jit_core::lift::CpuMode;
use x86jit_core::{Exit, MemoryModel, Prot, RegionKind, StepResult, Vm, VmConfig};

/// The corpus assumes a full 16 MB address space mapped and writable (real-mode
/// segment:offset only reaches ~1 MB+64 KB, but we map the whole space the rig
/// promises so no in-bounds access can ever miss). Page-aligned (0x1000000).
const GUEST_RAM: u64 = 0x0100_0000;

/// FLAGS bits our real-mode model tracks ([`Flags::to_flags16`]): CF(0), the always-1
/// reserved bit(1), PF(2), AF(4), ZF(6), SF(7), IF(9), DF(10), OF(11). TF(8), IOPL/NT
/// (12-15) and the reserved zero bits (3,5,13,15) are **not** modeled — `to_flags16`
/// reads them back as 0 while real hardware may push a randomized initial value — so
/// they are masked out of every comparison. This is the exact set of bits our model
/// is accountable for; the per-opcode undefined-flag mask narrows it further.
///
/// Stricter than the 8088 oracle, which omitted the reserved bit 1 — we assert it too
/// (both our model and the hardware hold it at 1).
const DEFINED_FLAGS: u16 = (1 << 0) // CF
    | (1 << 1)  // reserved, always 1
    | (1 << 2)  // PF
    | (1 << 4)  // AF
    | (1 << 6)  // ZF
    | (1 << 7)  // SF
    | (1 << 9)  // IF
    | (1 << 10) // DF
    | (1 << 11); // OF

// ---------------------------------------------------------------------------
// In-memory test shapes (decoded from MOO)
// ---------------------------------------------------------------------------

/// One corpus test. The per-cycle bus trace (`CYCL`) and queue state are skipped on
/// load — we validate final architectural state only, not the bus/prefetch trace.
#[derive(Debug, Clone)]
pub struct Ss286Test {
    pub idx: u32,
    pub name: String,
    /// Full instruction bytes incl. the terminating `HALT` (convenience; we execute
    /// from `initial.ram`).
    pub bytes: Vec<u8>,
    pub initial: Ss286State,
    pub final_: Ss286State,
    /// Present iff the instruction raised an exception. Carries the vector number and
    /// the linear address of the FLAGS word pushed on the stack (so its undefined bits
    /// can be masked in the memory compare).
    pub exception: Option<Ss286Exception>,
    /// SHA-1 of the original MOO test chunk; uniquely identifies a test (used to skip
    /// revoked tests, see [`RevocationList`]).
    pub hash: String,
}

/// A CPU + RAM snapshot. `ram` is `(linear_address, byte)` pairs across the space.
#[derive(Debug, Clone, Default)]
pub struct Ss286State {
    pub regs: Ss286Regs,
    pub ram: Vec<(u32, u8)>,
}

/// The real-mode register file. Every field is a 16-bit value; a `final` block omits
/// registers that did not change, so each is `Option` and the runner falls back to the
/// initial value when comparing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ss286Regs {
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

/// Exception record: the vector the CPU took and the linear address of the pushed
/// FLAGS word (`flag_address`; the pushed IP sits at `flag_address-4`, CS at
/// `flag_address-2`).
#[derive(Debug, Clone, Copy)]
pub struct Ss286Exception {
    pub number: u8,
    pub flag_address: u32,
}

// ---------------------------------------------------------------------------
// MOO binary parser
// ---------------------------------------------------------------------------

/// Register names in MOO `REGS`-bitmask order (bit `i` ⇒ `REG_ORDER[i]` present). Note
/// this is **not** the x86 GPR encoding order — [`load_regs`] maps by name.
const REG_ORDER: [&str; 14] = [
    "ax", "bx", "cx", "dx", "cs", "ss", "ds", "es", "sp", "bp", "si", "di", "ip", "flags",
];

/// A little-endian byte cursor over a MOO buffer. Reads panic on truncation — a
/// malformed corpus file is a hard error, not a skippable test.
struct Cur<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cur { buf, pos: 0 }
    }
    fn u8(&mut self) -> u8 {
        let v = self.buf[self.pos];
        self.pos += 1;
        v
    }
    fn u16(&mut self) -> u16 {
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes(self.buf[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }
    fn tag(&mut self) -> [u8; 4] {
        let v: [u8; 4] = self.buf[self.pos..self.pos + 4].try_into().unwrap();
        self.pos += 4;
        v
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        let v = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        v
    }
}

/// Decode a `REGS` sub-chunk body: a 16-bit presence bitmask, then one `u16` per set
/// bit in [`REG_ORDER`] order.
fn decode_regs(body: &[u8]) -> Ss286Regs {
    let mut c = Cur::new(body);
    let mask = c.u16();
    let mut r = Ss286Regs::default();
    for (i, name) in REG_ORDER.iter().enumerate() {
        if mask & (1 << i) != 0 {
            let v = Some(c.u16());
            match *name {
                "ax" => r.ax = v,
                "bx" => r.bx = v,
                "cx" => r.cx = v,
                "dx" => r.dx = v,
                "cs" => r.cs = v,
                "ss" => r.ss = v,
                "ds" => r.ds = v,
                "es" => r.es = v,
                "sp" => r.sp = v,
                "bp" => r.bp = v,
                "si" => r.si = v,
                "di" => r.di = v,
                "ip" => r.ip = v,
                "flags" => r.flags = v,
                _ => unreachable!(),
            }
        }
    }
    r
}

/// Decode a `RAM ` sub-chunk body: a `u32` count, then `count` × (`u32` address, `u8`
/// byte).
fn decode_ram(body: &[u8]) -> Vec<(u32, u8)> {
    let mut c = Cur::new(body);
    let count = c.u32() as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let addr = c.u32();
        let byte = c.u8();
        out.push((addr, byte));
    }
    out
}

/// Decode an `INIT`/`FINA` chunk body: a run of `REGS` / `RAM ` / `QUEU` sub-chunks
/// (`QUEU` — the prefetch queue — is skipped; we do not model it).
fn decode_state(body: &[u8]) -> Ss286State {
    let mut c = Cur::new(body);
    let mut st = Ss286State::default();
    while c.pos < body.len() {
        let tag = c.tag();
        let len = c.u32() as usize;
        let sub = c.take(len);
        match &tag {
            b"REGS" => st.regs = decode_regs(sub),
            b"RAM " => st.ram = decode_ram(sub),
            _ => {} // QUEU or any future chunk: skipped
        }
    }
    st
}

/// Parse a decompressed MOO buffer into tests, taking **at most** `limit` (0 = all).
/// The `CYCL` (per-cycle bus trace) sub-chunk is skipped by advancing past it, so the
/// bulk of each file is never materialised.
///
/// Layout: `b"MOO "` magic, a `u32` header length + header (version / test-count / CPU
/// name — validated but otherwise unused), then a stream of top-level chunks. Each
/// `TEST` chunk carries a `u32` index and interior `NAME`/`BYTS`/`INIT`/`FINA`/`CYCL`/
/// `HASH`/`EXCP` sub-chunks.
pub fn parse_moo(data: &[u8], limit: usize) -> Vec<Ss286Test> {
    let mut c = Cur::new(data);
    assert_eq!(&c.tag(), b"MOO ", "not a MOO file");
    let hlen = c.u32() as usize;
    let _header = c.take(hlen);

    let mut tests = Vec::new();
    while c.pos < data.len() {
        if limit != 0 && tests.len() >= limit {
            break;
        }
        let tag = c.tag();
        let len = c.u32() as usize;
        let body = c.take(len);
        if &tag != b"TEST" {
            continue; // unknown top-level chunk
        }

        let mut tc = Cur::new(body);
        let idx = tc.u32();
        let mut name = String::new();
        let mut bytes = Vec::new();
        let mut initial = Ss286State::default();
        let mut final_ = Ss286State::default();
        let mut exception = None;
        let mut hash = String::new();

        while tc.pos < body.len() {
            let subt = tc.tag();
            let slen = tc.u32() as usize;
            let sub = tc.take(slen);
            match &subt {
                b"NAME" => {
                    let mut s = Cur::new(sub);
                    let nl = s.u32() as usize;
                    name = String::from_utf8_lossy(s.take(nl)).into_owned();
                }
                b"BYTS" => {
                    let mut s = Cur::new(sub);
                    let cnt = s.u32() as usize;
                    bytes = s.take(cnt).to_vec();
                }
                b"INIT" => initial = decode_state(sub),
                b"FINA" => final_ = decode_state(sub),
                b"EXCP" => {
                    let mut s = Cur::new(sub);
                    let number = s.u8();
                    let flag_address = s.u32();
                    exception = Some(Ss286Exception {
                        number,
                        flag_address,
                    });
                }
                b"HASH" => hash = hex::encode(sub),
                _ => {} // CYCL and any future chunk: skipped
            }
        }

        tests.push(Ss286Test {
            idx,
            name,
            bytes,
            initial,
            final_,
            exception,
            hash,
        });
    }
    tests
}

// ---------------------------------------------------------------------------
// Opcode metadata (`metadata.json`) — undefined-flag masks
// ---------------------------------------------------------------------------

/// A node in `metadata.json`'s `opcodes` table: either a leaf opcode entry, or a group
/// with a `reg` subtable keyed by the modrm reg field (`"0".."7"`).
#[derive(Deserialize, Default, Clone)]
pub struct OpcodeMeta {
    #[serde(default)]
    pub status: Option<String>,
    /// 16-bit AND mask clearing the flags the CPU leaves undefined after this op.
    /// Absent ⇒ no undefined flags (mask = 0xFFFF).
    #[serde(rename = "flags-mask", default)]
    pub flags_mask: Option<u16>,
    /// modrm-`reg` subtable for grouped opcodes.
    #[serde(default)]
    pub reg: Option<BTreeMap<String, OpcodeMeta>>,
}

impl OpcodeMeta {
    fn mask(&self) -> u16 {
        self.flags_mask.unwrap_or(0xFFFF)
    }
}

/// The top of `metadata.json`. Only `opcodes` is consumed; the header fields
/// (version, CPU detail, …) are ignored.
#[derive(Deserialize)]
struct MetadataFile {
    opcodes: BTreeMap<String, OpcodeMeta>,
}

/// The opcode-metadata table, opcode-hex-string keyed.
pub struct Metadata(BTreeMap<String, OpcodeMeta>);

impl Metadata {
    /// The undefined-flag AND mask for `opcode`, descending into the `reg` subtable by
    /// the modrm `reg` field when the opcode is a group (`reg = None` for a plain op).
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
// Revocation list
// ---------------------------------------------------------------------------

/// Hashes of tests the upstream suite has retracted as inaccurate
/// (`revocation_list.txt`, one hex SHA-1 per line). Revoked tests are skipped.
#[derive(Default)]
pub struct RevocationList(std::collections::HashSet<String>);

impl RevocationList {
    pub fn contains(&self, hash: &str) -> bool {
        !self.0.is_empty() && self.0.contains(hash)
    }
}

// ---------------------------------------------------------------------------
// Corpus discovery / loading
// ---------------------------------------------------------------------------

/// Root of the fetched corpus (`vendor/80286/v1_real_mode`), or `None` when it has not
/// been fetched. Resolved relative to this crate's manifest dir so it works from any
/// cwd.
pub fn corpus_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/80286/v1_real_mode");
    dir.is_dir().then_some(dir)
}

/// Load the `metadata.json` opcode metadata from the corpus dir.
pub fn load_metadata(dir: &Path) -> Metadata {
    let text = std::fs::read_to_string(dir.join("metadata.json"))
        .expect("metadata.json present in corpus");
    let file: MetadataFile = serde_json::from_str(&text).expect("metadata.json parses");
    Metadata(file.opcodes)
}

/// Load the optional `revocation_list.txt` (absent ⇒ empty list).
pub fn load_revocations(dir: &Path) -> RevocationList {
    match std::fs::read_to_string(dir.join("revocation_list.txt")) {
        Ok(text) => RevocationList(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        Err(_) => RevocationList::default(),
    }
}

/// The per-opcode test files present in the corpus dir, sorted. Each entry is
/// `(file_stem, path)` where `file_stem` is e.g. `"00"` or `"F7.6"` — the leading two
/// hex digits are the opcode; a `.R` suffix is the modrm-reg group member.
pub fn opcode_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".MOO.gz") {
            out.push((stem.to_string(), entry.path()));
        }
    }
    out.sort();
    out
}

/// Parse the opcode byte and optional modrm-`reg` group member from a file stem like
/// `"00"` or `"F7.6"`.
pub fn parse_stem(stem: &str) -> Option<(u8, Option<u8>)> {
    let (hex, reg) = match stem.split_once('.') {
        Some((h, r)) => (h, r.parse::<u8>().ok()),
        None => (stem, None),
    };
    let opcode = u8::from_str_radix(hex, 16).ok()?;
    Some((opcode, reg))
}

/// Decompress and MOO-parse an opcode file, taking at most `limit` tests (0 = all).
pub fn load_tests(path: &Path, limit: usize) -> Vec<Ss286Test> {
    use std::io::Read;
    let file = std::fs::File::open(path).expect("opcode file opens");
    let mut gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut data = Vec::new();
    gz.read_to_end(&mut data).expect("gunzip opcode file");
    parse_moo(&data, limit)
}

// ---------------------------------------------------------------------------
// The oracle: replay one test on the Real16 interpreter
// ---------------------------------------------------------------------------

/// Outcome of running a single corpus test.
#[derive(Debug, PartialEq, Eq)]
pub enum TestOutcome {
    /// Final CPU + RAM state matched (after flag masking).
    Pass,
    /// The interpreter ran to the terminating `HALT` but reached a different final
    /// state. Carries a short human diff for the first divergence.
    Fail(String),
    /// The Real16 lifter does not yet support this opcode (`Exit::UnknownInstruction`).
    /// Counted per-opcode, never silently passed.
    Unsupported,
    /// The interpreter trapped out in a way that is neither an unsupported opcode nor a
    /// clean terminating `HALT` (unmapped memory, budget exhaustion, an interpreter that
    /// ran off into unseeded RAM, …). Kept distinct from `Fail` so genuine surprises
    /// stand out.
    Trapped(String),
    /// The instruction is `IN`/`OUT`: it surfaces as [`Exit::PortIo`] for the embedder
    /// to service. This oracle mounts no port device, so such a test is **not
    /// executable here** — it is neither a pass nor a model divergence, and is excluded
    /// from the decided-pass-rate (like [`TestOutcome::Unsupported`]).
    PortIo,
}

/// A reusable Real16 execution harness owning one 16 MB flat `Vm` across many tests.
/// Allocating a fresh buffer per test would dominate the run, so [`Runner::run`]
/// re-seeds only the RAM each test touches and afterwards zeroes exactly the union of
/// the addresses it read or wrote, leaving the buffer clean for the next test without a
/// re-allocation. Correct because the corpus lists every address an instruction reads
/// in `initial.ram` and every address it writes in `final.ram`.
pub struct Runner {
    vm: Vm,
    /// Scratch: addresses to zero after a test (its `initial.ram ∪ final.ram`).
    touched: Vec<u64>,
}

/// Upper bound on single-steps per test before we call it a runaway. A test is the
/// instruction under test (one step; a `REP` string op with `CX` masked to 7 bits still
/// retires in one `step_instruction`) plus the terminating `HALT`. A generous cap
/// tolerates any prefix-as-separate-instruction decode while still catching an
/// interpreter that never reaches the `HALT`.
const MAX_STEPS: usize = 64;

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner {
    /// Build the reusable Real16 VM with a full 16 MB of flat RAM mapped RWX.
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
            .expect("16 MB flat maps");
        Runner {
            vm,
            touched: Vec::new(),
        }
    }

    /// Replay one corpus test on the interpreter in `CpuMode::Real16` and classify the
    /// outcome. `flags_mask` is the opcode's undefined-flag AND mask from the metadata.
    ///
    /// Executes to the terminating `HALT` (the corpus records final state *after* the
    /// `HALT` retires; see the module docs). A real-mode `#DE`/`int`/`ud2` vectors
    /// in-guest through the IVT and returns `Continue`, so a faulting instruction runs
    /// on into the injected handler `HALT`.
    pub fn run(&mut self, test: &Ss286Test, flags_mask: u16) -> TestOutcome {
        self.touched.clear();
        for &(addr, byte) in &test.initial.ram {
            self.vm
                .write_bytes(addr as u64, &[byte])
                .expect("ram address within 16 MB");
            self.touched.push(addr as u64);
        }

        let mut cpu = self.vm.new_vcpu();
        load_regs(&mut cpu.cpu, &test.initial.regs);

        // Step until the terminating HALT retires (Exit::Hlt advances RIP past it).
        let mut outcome = None;
        for _ in 0..MAX_STEPS {
            match cpu.step_instruction(&self.vm) {
                StepResult::Continue => continue,
                StepResult::Exit(Exit::Hlt) => {
                    outcome = Some(self.compare(test, flags_mask, &cpu));
                    break;
                }
                StepResult::Exit(Exit::UnknownInstruction { .. }) => {
                    outcome = Some(TestOutcome::Unsupported);
                    break;
                }
                // IN/OUT: not executable without a port device (see `PortIo`).
                StepResult::Exit(Exit::PortIo { .. }) => {
                    outcome = Some(TestOutcome::PortIo);
                    break;
                }
                StepResult::Exit(other) => {
                    outcome = Some(TestOutcome::Trapped(format!("{other:?}")));
                    break;
                }
            }
        }
        let outcome =
            outcome.unwrap_or_else(|| TestOutcome::Trapped(format!("no HALT within {MAX_STEPS}")));

        // Restore the buffer to all-zero over exactly what this test touched.
        for &(addr, _) in &test.final_.ram {
            self.touched.push(addr as u64);
        }
        for &addr in &self.touched {
            self.vm.write_bytes(addr, &[0u8]).expect("addr mapped");
        }
        outcome
    }

    /// Diff the architecturally defined final CPU + RAM state against the corpus's
    /// `final` block, applying the flag mask. For an exception test the FLAGS word the
    /// CPU pushed to the stack is masked the same way as the FLAGS register (it too
    /// carries the op's undefined bits and our unmodeled system bits).
    fn compare(&self, test: &Ss286Test, flags_mask: u16, cpu: &x86jit_core::Vcpu) -> TestOutcome {
        let mut diffs: Vec<String> = Vec::new();
        let exp = &test.final_.regs;
        let init = &test.initial.regs;
        let cmp =
            |name: &str, exp: Option<u16>, init: Option<u16>, got: u16, diffs: &mut Vec<String>| {
                // A `final` register absent from the file kept its initial value.
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

        // Flags: mask off (a) bits our model doesn't track and (b) the bits this op
        // leaves undefined, then compare only what's left.
        let mask = DEFINED_FLAGS & flags_mask;
        let want_flags = exp.flags.or(init.flags).unwrap_or(0) & mask;
        let got_flags = cpu.cpu.flags.to_flags16() & mask;
        if want_flags != got_flags {
            diffs.push(format!(
                "flags: want {want_flags:#06x} got {got_flags:#06x} (mask {mask:#06x})"
            ));
        }

        // Memory: every byte `final.ram` lists must read back to that value — except
        // the two bytes of the FLAGS word an exception pushed, which carry the op's
        // undefined flag bits and our unmodeled system bits and so are flag-masked. The
        // pushed IP and CS (at flag_address-4 / -2) are checked exactly: a wrong saved
        // return frame (e.g. 8086-style next-IP instead of the 286 fault IP) is a real
        // divergence, not something to mask.
        let flag_lo = test.exception.map(|e| e.flag_address);
        for &(addr, byte) in &test.final_.ram {
            let mut buf = [0u8];
            self.vm
                .read_bytes(addr as u64, &mut buf)
                .expect("final ram address mapped");
            let (want, got) = if Some(addr) == flag_lo {
                let m = (mask & 0xFF) as u8;
                (byte & m, buf[0] & m)
            } else if Some(addr) == flag_lo.map(|a| a + 1) {
                let m = (mask >> 8) as u8;
                (byte & m, buf[0] & m)
            } else {
                (byte, buf[0])
            };
            if want != got {
                diffs.push(format!("mem[{addr:#08x}]: want {want:#04x} got {got:#04x}"));
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
/// hand-written fixtures; the corpus sweep reuses one `Runner`.
pub fn run_test(test: &Ss286Test, flags_mask: u16) -> TestOutcome {
    Runner::new().run(test, flags_mask)
}

/// Seed the interpreter's `CpuState` from a corpus register block. Maps register
/// **names** to x86 GPR encoding order (AX=0, CX=1, DX=2, BX=3, SP=4, BP=5, SI=6,
/// DI=7) — note the corpus lists them in a different order.
fn load_regs(cpu: &mut x86jit_core::state::CpuState, r: &Ss286Regs) {
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

/// Per-opcode outcome counts, plus a sample failure and exception coverage.
#[derive(Default)]
pub struct OpTally {
    pub pass: u64,
    pub fail: u64,
    pub unsupported: u64,
    pub trapped: u64,
    /// `IN`/`OUT` tests not executable without a port device (see [`TestOutcome::PortIo`]).
    pub port_io: u64,
    /// Tests in this opcode that carried an exception record …
    pub exc_total: u64,
    /// … and how many of those passed (exception frame + vector validated).
    pub exc_pass: u64,
    /// First failing/trapped test's name + diff, for the gap report.
    pub sample: Option<String>,
}

impl OpTally {
    /// All tests recorded for this opcode, across every outcome.
    pub fn total(&self) -> u64 {
        self.pass + self.fail + self.unsupported + self.trapped + self.port_io
    }
    /// The **decided** tests: those the oracle could actually run and judge. Excludes
    /// opcodes the lifter does not support at all and `IN`/`OUT` (no port device).
    pub fn decided(&self) -> u64 {
        self.pass + self.fail + self.trapped
    }
}

/// The whole run's tally, keyed by opcode file stem (`"00"`, `"F7.6"`, …), plus an
/// exception-delivery breakdown by vector number.
#[derive(Default)]
pub struct Summary {
    pub by_op: BTreeMap<String, OpTally>,
    /// Per-exception-vector `(passed, total)` across all opcodes. Shows which vectors
    /// we deliver correctly (`#DE`, `int3`, …) versus not (e.g. the 286 segment-limit
    /// `#GP` we do not yet model).
    pub exc_by_vec: BTreeMap<u8, (u64, u64)>,
}

impl Summary {
    pub fn record(&mut self, stem: &str, test: &Ss286Test, outcome: TestOutcome) {
        let t = self.by_op.entry(stem.to_string()).or_default();
        let exc_vec = test.exception.map(|e| e.number);
        if exc_vec.is_some() {
            t.exc_total += 1;
        }
        let passed = matches!(outcome, TestOutcome::Pass);
        match outcome {
            TestOutcome::Pass => {
                t.pass += 1;
                if exc_vec.is_some() {
                    t.exc_pass += 1;
                }
            }
            TestOutcome::Unsupported => t.unsupported += 1,
            TestOutcome::PortIo => t.port_io += 1,
            TestOutcome::Fail(d) => {
                t.fail += 1;
                if t.sample.is_none() {
                    t.sample = Some(format!("`{}`: {d}", test.name));
                }
            }
            TestOutcome::Trapped(d) => {
                t.trapped += 1;
                if t.sample.is_none() {
                    t.sample = Some(format!("`{}` TRAP: {d}", test.name));
                }
            }
        }
        if let Some(v) = exc_vec {
            let e = self.exc_by_vec.entry(v).or_default();
            e.1 += 1;
            if passed {
                e.0 += 1;
            }
        }
    }

    pub fn totals(&self) -> OpTally {
        let mut acc = OpTally::default();
        for t in self.by_op.values() {
            acc.pass += t.pass;
            acc.fail += t.fail;
            acc.unsupported += t.unsupported;
            acc.trapped += t.trapped;
            acc.port_io += t.port_io;
            acc.exc_total += t.exc_total;
            acc.exc_pass += t.exc_pass;
        }
        acc
    }
}
