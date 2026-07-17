//! `cargo xfuzz` — the AVX/VEX differential fuzz CLI (task-267).
//!
//! Point it at a specific instruction (or family, or the whole pool) without env-var
//! incantations or editing source. Every leg (JIT-vs-interp, native-vs-interp) runs from a
//! real binary that links the JIT backend; findings print a copy-paste reproducer.
//!
//! ```text
//! cargo xfuzz --list                       # families + op names + counts
//! cargo xfuzz --ops vcvtps2ph              # subset the pool to one op
//! cargo xfuzz --ops vpaddsb,vpaddsw        # subset to several
//! cargo xfuzz --family convert,fma         # subset by family
//! cargo xfuzz --class vex --secs 3600 --len 12
//! cargo xfuzz --seed 1964 --ops vcvtps2ph  # replay one program deterministically
//! cargo xfuzz                              # bounded 60s smoke over everything
//! ```
//!
//! The alias lives in `.cargo/config.toml`; `cargo run -p x86jit-tests --bin fuzz -- …` works
//! too. Named `xfuzz`, not `fuzz`, to avoid shadowing cargo-fuzz (libfuzzer).

use std::path::PathBuf;
use std::process::ExitCode;

use x86jit_tests::fuzz::{
    all_ops, print_report, resolve_families, resolve_ops, run_campaign, CampaignCfg, Family, V_VEX,
};

const USAGE: &str = "\
cargo xfuzz — AVX/VEX differential fuzz CLI

USAGE:
  cargo xfuzz [OPTIONS]

POOL SELECTION (subsets the pool BEFORE generation):
  --ops <a,b,..>      only these op names (see --list)
  --family <f,g,..>   only these families (see --list)
  --class <vex>       instruction class (only 'vex' is supported; the default)

RUN:
  --secs <N>          time budget in seconds (default 60)
  --len <N>           program length in instructions (default 12)
  --start <N>         first seed (default 1)
  --seed <N>          replay exactly one program (seed N) and stop
  --log <PATH>        findings log file (default fuzz-avx-findings.log; none for --seed)
  --no-log            disable the findings log
  --quiet             suppress live per-finding output

INFO:
  --list, -l          print every op grouped by family, with counts, and exit
  --help, -h          this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut ops_arg: Option<String> = None;
    let mut family_arg: Option<String> = None;
    let mut class_arg: Option<String> = None;
    let mut secs: u64 = 60;
    let mut len: usize = 12;
    let mut start: u64 = 1;
    let mut seed: Option<u64> = None;
    let mut log_arg: Option<String> = None;
    let mut no_log = false;
    let mut quiet = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        macro_rules! val {
            ($flag:expr) => {
                match it.next() {
                    Some(v) => v.clone(),
                    None => {
                        eprintln!("error: {} requires a value", $flag);
                        return ExitCode::from(2);
                    }
                }
            };
        }
        macro_rules! num {
            ($flag:expr) => {{
                let v = val!($flag);
                match v.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!("error: {} expects a number, got {:?}", $flag, v);
                        return ExitCode::from(2);
                    }
                }
            }};
        }
        match a.as_str() {
            "--list" | "-l" => {
                print_list();
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--ops" => ops_arg = Some(val!("--ops")),
            "--family" => family_arg = Some(val!("--family")),
            "--class" => class_arg = Some(val!("--class")),
            "--secs" => secs = num!("--secs"),
            "--len" => len = num!("--len"),
            "--start" => start = num!("--start"),
            "--seed" => seed = Some(num!("--seed")),
            "--log" => log_arg = Some(val!("--log")),
            "--no-log" => no_log = true,
            "--quiet" => quiet = true,
            other => {
                eprintln!("error: unknown argument {other:?}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    // Only the `vex` class exists today (the AVX2 VEX pool). Accept it; reject others.
    if let Some(c) = &class_arg {
        if c != "vex" {
            eprintln!("error: unknown --class {c:?} (only 'vex' is supported)");
            return ExitCode::from(2);
        }
    }

    if ops_arg.is_some() && family_arg.is_some() {
        eprintln!("error: pass --ops or --family, not both");
        return ExitCode::from(2);
    }

    // Resolve the pool subset (before generation).
    let vex_ops = if let Some(names) = &ops_arg {
        match resolve_ops(names) {
            Ok(v) => v,
            Err(bad) => {
                eprintln!("error: unknown op {bad:?} (see `cargo xfuzz --list`)");
                return ExitCode::from(2);
            }
        }
    } else if let Some(fams) = &family_arg {
        match resolve_families(fams) {
            Ok(v) => v,
            Err(bad) => {
                eprintln!("error: unknown family {bad:?} (see `cargo xfuzz --list`)");
                return ExitCode::from(2);
            }
        }
    } else {
        all_ops()
    };

    // Reproducer prefix: the pool-selecting args of THIS run, so `<prefix> --seed N`
    // regenerates the same program deterministically.
    let mut repro_prefix = String::from("cargo xfuzz");
    if let Some(names) = &ops_arg {
        repro_prefix.push_str(&format!(" --ops {names}"));
    } else if let Some(fams) = &family_arg {
        repro_prefix.push_str(&format!(" --family {fams}"));
    }
    if len != 12 {
        repro_prefix.push_str(&format!(" --len {len}"));
    }

    // Log: default file for campaigns; none for a single-seed replay unless asked.
    let log_path: Option<PathBuf> = if no_log {
        None
    } else if let Some(p) = log_arg {
        Some(PathBuf::from(p))
    } else if seed.is_some() {
        None
    } else {
        Some(PathBuf::from("fuzz-avx-findings.log"))
    };

    let cfg = CampaignCfg {
        secs,
        len,
        start_seed: start,
        single: seed,
        vex_ops,
        log_path,
        repro_prefix,
        status: seed.is_none(),
        quiet,
    };

    if seed.is_none() {
        println!(
            "cargo xfuzz: {} op(s), len={}, secs={}, native oracle {}",
            cfg.vex_ops.len(),
            cfg.len,
            cfg.secs,
            if x86jit_tests::fuzz::native_available() {
                "on"
            } else {
                "OFF (not x86-64/Linux)"
            }
        );
    }

    let report = run_campaign(&cfg);

    if seed.is_none() {
        print_report(&report);
    }

    // Non-zero exit if any divergence was found, so CI / scripts can gate on it.
    if report.findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Print every op grouped by family, with per-family and total counts.
fn print_list() {
    println!(
        "VEX/AVX2 fuzz op pool — {} ops across {} families",
        V_VEX.len(),
        Family::ALL.len()
    );
    for fam in Family::ALL {
        let ops: Vec<&str> = V_VEX
            .iter()
            .filter(|o| o.family == fam)
            .map(|o| o.name)
            .collect();
        if ops.is_empty() {
            continue;
        }
        println!("  [{}] ({} ops)", fam.name(), ops.len());
        println!("    {}", ops.join(" "));
    }
}
