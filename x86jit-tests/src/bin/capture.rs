//! `capture` — snippet → vector, using Unicorn as the oracle (testing.md §6.1).
//!
//! Assembles/loads a code snippet, runs it through the `UnicornOracle`, and
//! writes a `.ron` vector that is a permanent regression test replayable without
//! an oracle. Requires the `unicorn` feature.
//!
//! ```text
//! cargo run -p x86jit-tests --features unicorn --bin capture -- \
//!   --bytes 01d8f4 --init "rax=0xffffffff00000001,rbx=2" \
//!   --name add_r32_zeroes_upper --tags flags,zero-extend \
//!   --note "writing eax zeroes the upper 32 bits of rax" --out vectors/zero_extend/
//! ```
//!
//! `--data 0xADDR=HEX` adds a RW data/stack region (repeatable). `--dont-care`
//! lists undefined flags to mask (e.g. `af`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use x86jit_tests::oracle::{Oracle, VectorInput};
use x86jit_tests::unicorn::UnicornOracle;
use x86jit_tests::vector::{
    CpuSnapshot, Expectation, FlagName, MemChunk, MemKind, RunSpec, TestVector,
};

fn main() {
    let args = Args::parse(std::env::args().skip(1));

    let mut mem_init = vec![MemChunk {
        addr: args.entry,
        bytes: args.code.clone(),
        kind: MemKind::Ram,
    }];
    for (addr, bytes) in &args.data {
        mem_init.push(MemChunk { addr: *addr, bytes: bytes.clone(), kind: MemKind::Ram });
    }

    let input = VectorInput {
        cpu_init: args.cpu_init.clone(),
        mem_init: mem_init.clone(),
        entry: args.entry,
        run: RunSpec::UntilExit,
    };

    let outcome = UnicornOracle.run(&input);

    // mem_diff = the FINAL content of each region that changed (whole region).
    let mem_diff = mem_init
        .iter()
        .zip(&outcome.mem)
        .filter(|(init, fin)| init.bytes != fin.bytes)
        .map(|(_, fin)| fin.clone())
        .collect();

    let vector = TestVector {
        name: args.name.clone(),
        note: args.note,
        tags: args.tags,
        cpu_init: args.cpu_init,
        mem_init,
        entry: args.entry,
        run: RunSpec::UntilExit,
        expect: Expectation { cpu: outcome.cpu, mem_diff, exit: outcome.exit },
        dont_care_flags: args.dont_care,
    };

    let mut path = args.out;
    std::fs::create_dir_all(&path).expect("create out dir");
    path.push(format!("{}.ron", args.name));
    std::fs::write(&path, vector.to_ron()).expect("write vector");
    println!("wrote {}", path.display());
}

struct Args {
    code: Vec<u8>,
    data: Vec<(u64, Vec<u8>)>,
    entry: u64,
    cpu_init: CpuSnapshot,
    name: String,
    note: String,
    tags: Vec<String>,
    dont_care: Vec<FlagName>,
    out: PathBuf,
}

impl Args {
    fn parse(mut it: impl Iterator<Item = String>) -> Self {
        let mut code = None;
        let mut data = Vec::new();
        let mut entry = 0x1000u64;
        let mut init = String::new();
        let mut name = None;
        let mut note = String::new();
        let mut tags = Vec::new();
        let mut dont_care = Vec::new();
        let mut out = PathBuf::from("vectors/found/");

        while let Some(flag) = it.next() {
            let mut val = || it.next().expect("flag needs a value");
            match flag.as_str() {
                "--bytes" => code = Some(hex::decode(val().replace(' ', "")).expect("hex code")),
                "--entry" => entry = parse_u64(&val()),
                "--init" => init = val(),
                "--data" => {
                    let s = val();
                    let (a, b) = s.split_once('=').expect("--data ADDR=HEX");
                    data.push((parse_u64(a), hex::decode(b).expect("hex data")));
                }
                "--name" => name = Some(val()),
                "--note" => note = val(),
                "--tags" => tags = val().split(',').map(|s| s.trim().to_string()).collect(),
                "--dont-care" => {
                    dont_care = val().split(',').filter_map(|s| flag_name(s.trim())).collect()
                }
                "--out" => out = PathBuf::from(val()),
                other => panic!("unknown flag: {other}"),
            }
        }

        Args {
            code: code.expect("--bytes required"),
            data,
            entry,
            cpu_init: parse_init(&init, entry),
            name: name.expect("--name required"),
            note,
            tags,
            dont_care,
            out,
        }
    }
}

fn parse_init(s: &str, entry: u64) -> CpuSnapshot {
    let mut snap = CpuSnapshot { rip: entry, ..Default::default() };
    let names: BTreeMap<&str, usize> = [
        "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
        "r13", "r14", "r15",
    ]
    .iter()
    .enumerate()
    .map(|(i, &n)| (n, i))
    .collect();

    for pair in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (reg, val) = pair.split_once('=').expect("init: reg=value");
        let (reg, val) = (reg.trim().to_lowercase(), parse_u64(val.trim()));
        match reg.as_str() {
            "fs_base" => snap.fs_base = val,
            "gs_base" => snap.gs_base = val,
            r => {
                let idx = names.get(r).unwrap_or_else(|| panic!("unknown reg {r}"));
                snap.gpr[*idx] = val;
            }
        }
    }
    snap
}

fn parse_u64(s: &str) -> u64 {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).expect("hex number")
    } else {
        s.parse().expect("decimal number")
    }
}

fn flag_name(s: &str) -> Option<FlagName> {
    Some(match s {
        "cf" => FlagName::Cf,
        "pf" => FlagName::Pf,
        "af" => FlagName::Af,
        "zf" => FlagName::Zf,
        "sf" => FlagName::Sf,
        "of" => FlagName::Of,
        "df" => FlagName::Df,
        _ => return None,
    })
}
