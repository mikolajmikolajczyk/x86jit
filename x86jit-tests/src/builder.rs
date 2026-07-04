//! Inline vector builder (testing.md §6.2) — the ergonomic path for hand-crafted
//! cases:
//!
//! ```ignore
//! Vector::asm(|a| { a.add(eax, ebx).unwrap(); a.hlt().unwrap(); })
//!     .init(|c| { c.gpr[0] = 0xFFFF_FFFF_0000_0001; c.gpr[3] = 2; })
//!     .dont_care(&[FlagName::Af])
//!     .assert_matches_unicorn();          // needs the `unicorn` feature
//! ```
//!
//! Assembles at a fixed entry with a scratch RW page auto-mapped (for stack /
//! data). `interpret()` runs the engine under test; `assert_matches_unicorn()`
//! is the differential check.

use iced_x86::code_asm::CodeAssembler;

use crate::oracle::{InterpreterOracle, Oracle, RunOutcome, VectorInput};
use crate::vector::{CpuSnapshot, FlagName, MemChunk, MemKind, RunSpec};

const ENTRY: u64 = 0x1000;
const SCRATCH: u64 = 0x8000;
const SCRATCH_LEN: usize = 0x1000;

pub struct Vector {
    cpu_init: CpuSnapshot,
    mem_init: Vec<MemChunk>,
    dont_care: Vec<FlagName>,
}

impl Vector {
    /// Build a snippet with the iced code assembler. A scratch RW page is mapped
    /// at [`Vector::scratch`]; point RSP into it for stack ops.
    pub fn asm(build: impl FnOnce(&mut CodeAssembler)) -> Self {
        let mut asm = CodeAssembler::new(64).unwrap();
        build(&mut asm);
        let code = asm.assemble(ENTRY).unwrap();
        Self {
            cpu_init: CpuSnapshot {
                rip: ENTRY,
                ..Default::default()
            },
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
            dont_care: Vec::new(),
        }
    }

    pub fn init(mut self, f: impl FnOnce(&mut CpuSnapshot)) -> Self {
        f(&mut self.cpu_init);
        self
    }

    pub fn data(mut self, addr: u64, bytes: Vec<u8>) -> Self {
        self.mem_init.push(MemChunk {
            addr,
            bytes,
            kind: MemKind::Ram,
        });
        self
    }

    pub fn dont_care(mut self, flags: &[FlagName]) -> Self {
        self.dont_care.extend_from_slice(flags);
        self
    }

    /// A mid-scratch address, a convenient initial RSP.
    pub fn scratch() -> u64 {
        SCRATCH + 0x800
    }

    fn input(&self) -> VectorInput {
        VectorInput {
            cpu_init: self.cpu_init.clone(),
            mem_init: self.mem_init.clone(),
            entry: ENTRY,
            run: RunSpec::UntilExit,
        }
    }

    /// Run the engine under test (interpreter).
    pub fn interpret(&self) -> RunOutcome {
        InterpreterOracle.run(&self.input())
    }

    /// Differential check: the interpreter must match Unicorn (masking undefined
    /// flags). Panics with a precise divergence report on mismatch.
    #[cfg(feature = "unicorn")]
    pub fn assert_matches_unicorn(&self) {
        use crate::compare::compare;
        use crate::unicorn::UnicornOracle;

        let interp = self.interpret();
        let unicorn = UnicornOracle.run(&self.input());
        if let Some(d) = compare(&unicorn, &interp, &self.dont_care) {
            panic!("interpreter diverges from Unicorn:\n{d}");
        }
    }
}
