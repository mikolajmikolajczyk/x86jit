//! TomHarte / SingleStepTests **8088** corpus as a Real16 CPU oracle.
//!
//! Replays each fetched per-opcode test file on the x86jit interpreter in
//! `CpuMode::Real16` and asserts the architecturally defined final state matches
//! real-8088-captured truth. See [`x86jit_tests::harte`] for the loader + the
//! flag-masking policy.
//!
//! # Running
//! The corpus is large and gitignored. Fetch it first:
//! ```text
//! x86jit-tests/vendor/8088/fetch.sh          # full corpus (~800 MB)
//! x86jit-tests/vendor/8088/fetch.sh 00 01 D0.4  # a subset
//! ```
//! With the corpus absent, [`corpus_runs`] **skips with a message** and
//! [`fixtures_exercise_loader`] still runs (hand-written cases in the corpus shape).
//!
//! `HARTE_LIMIT=N` caps tests-per-opcode (default: all 10 000) for a faster sweep;
//! the CI/full run leaves it unset. `HARTE_VERBOSE=1` prints per-opcode lines.

use x86jit_tests::harte::{
    self, run_test, HarteRegs, HarteState, HarteTest, Runner, Summary, TestOutcome,
};

/// Per-opcode cap, from `HARTE_LIMIT` (0/unset = all tests in the file).
fn limit() -> usize {
    std::env::var("HARTE_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[test]
fn corpus_runs() {
    let Some(dir) = harte::corpus_dir() else {
        eprintln!(
            "SKIP harte8088::corpus_runs — corpus not fetched.\n\
             Run x86jit-tests/vendor/8088/fetch.sh to pull it (gitignored, ~800 MB).\n\
             CI must fetch the corpus before this test is meaningful."
        );
        return;
    };

    let meta = harte::load_metadata(&dir);
    let files = harte::opcode_files(&dir);
    assert!(
        !files.is_empty(),
        "corpus dir present but holds no opcode files"
    );

    let cap = limit();
    let verbose = std::env::var_os("HARTE_VERBOSE").is_some();
    let mut summary = Summary::default();
    // One reusable 1 MB Real16 VM for the whole sweep (see `Runner`).
    let mut runner = Runner::new();

    for (stem, path) in &files {
        let Some((opcode, reg)) = harte::parse_stem(stem) else {
            continue;
        };
        let mask = meta.flags_mask(opcode, reg);
        // Streaming, cap-aware load: parses at most `cap` tests (0 = all), skipping the
        // large per-cycle bus trace we do not model.
        let tests = harte::load_tests(path, cap);
        for t in &tests {
            let outcome = runner.run(t, mask);
            summary.record(stem, &t.name, outcome);
        }
        if verbose {
            let t = &summary.by_op[stem];
            eprintln!(
                "  {stem}: pass {} fail {} trapped {} unsupported {} addr_wrap {}",
                t.pass, t.fail, t.trapped, t.unsupported, t.addr_wrap
            );
        }
    }

    print_summary(&summary);

    // This test is a **reporting oracle + regression tripwire**, not a 100%-conformance
    // gate. The corpus surfaces a real gap list — unlifted opcodes (`unsupported`), the
    // unmodeled 20-bit address wrap (`addr_wrap`), and genuine interpreter divergences
    // (`fail`/`trapped`) — that drives follow-up lift work; forcing them all green would
    // hide exactly what we want to see. So we do NOT assert a high pass rate.
    //
    // We DO assert a low floor on the *decided* tests (pass + fail + trapped; excludes
    // opcodes not lifted at all and the documented address-wrap gap) so a future change
    // that badly regresses the interpreter turns this red. The current executed pass
    // rate is ~0.95+; the 0.80 floor is slack enough to never flake on the existing gap
    // list yet catch a real collapse. The full gap list is printed above regardless.
    let tot = summary.totals();
    let decided = tot.pass + tot.fail + tot.trapped;
    if decided > 0 {
        let rate = tot.pass as f64 / decided as f64;
        assert!(
            rate >= 0.80,
            "decided-test pass rate {rate:.4} below regression floor 0.80 \
             ({} pass / {} fail / {} trapped of {decided} decided); see gap list above",
            tot.pass,
            tot.fail,
            tot.trapped,
        );
    }
}

/// Human-readable summary: overall + decided pass rate, then a per-opcode gap list of
/// failing / trapped / unsupported / address-wrap opcodes (the follow-up lift work).
fn print_summary(s: &Summary) {
    let tot = s.totals();
    let total = tot.total();
    eprintln!("\n=== TomHarte 8088 Real16 oracle — summary ===");
    eprintln!(
        "total {total}  pass {}  fail {}  trapped {}  unsupported {}  addr_wrap {}",
        tot.pass, tot.fail, tot.trapped, tot.unsupported, tot.addr_wrap
    );
    if total > 0 {
        let decided = tot.pass + tot.fail + tot.trapped;
        eprintln!(
            "overall pass rate: {:.4}   decided pass rate: {:.4}  \
             (decided = pass+fail+trapped; excludes unsupported + addr_wrap)",
            tot.pass as f64 / total as f64,
            if decided == 0 {
                1.0
            } else {
                tot.pass as f64 / decided as f64
            },
        );
    }

    let mut gaps: Vec<(&String, &harte::OpTally)> = s
        .by_op
        .iter()
        .filter(|(_, t)| t.fail > 0 || t.trapped > 0 || t.unsupported > 0 || t.addr_wrap > 0)
        .collect();
    gaps.sort_by_key(|(stem, _)| (*stem).clone());
    if !gaps.is_empty() {
        eprintln!("\n--- gap list (opcode: pass/fail/trapped/unsupported/addr_wrap of total) ---");
        for (stem, t) in gaps {
            eprint!(
                "  {stem}: {}/{}/{}/{}/{} of {}",
                t.pass,
                t.fail,
                t.trapped,
                t.unsupported,
                t.addr_wrap,
                t.total()
            );
            if let Some(sample) = &t.sample {
                eprint!("   e.g. {sample}");
            }
            eprintln!();
        }
    }
}

/// Even without the corpus, exercise the loader + runner on hand-written cases in the
/// exact corpus JSON shape, so the parse → Real16-run → compare path is always tested.
#[test]
fn fixtures_exercise_loader() {
    // `inc ax` (opcode 0x40) at CS:IP = 0x1000:0x0000 ⇒ linear 0x10000. INC does not
    // touch CF; sets ZF/SF/PF/AF/OF from the result. 0x00FF+1 = 0x0100.
    let inc_ax = HarteTest {
        name: "inc ax (fixture)".into(),
        bytes: vec![0x40],
        initial: HarteState {
            regs: HarteRegs {
                ax: Some(0x00FF),
                cs: Some(0x1000),
                ip: Some(0x0000),
                flags: Some(0x0002), // only reserved bit set
                ..Default::default()
            },
            ram: vec![[0x10000, 0x40], [0x10001, 0x90]],
        },
        final_: HarteState {
            regs: HarteRegs {
                ax: Some(0x0100),
                ip: Some(0x0001),
                // AF set (nibble carry), PF set (0x00 low byte), CF untouched.
                flags: Some(0x0002 | (1 << 4) | (1 << 2)),
                ..Default::default()
            },
            ram: vec![],
        },
    };
    assert_eq!(run_test(&inc_ax, 0xFFFF), TestOutcome::Pass);

    // `mov byte [ds:0x0020], 0xAB` — opcode 0xC6 /0. Writes memory, touches no flags.
    // Encoding: C6 06 20 00 AB (modrm 06 = disp16 direct). DS=0x0000 ⇒ linear 0x20.
    let mov_mem = HarteTest {
        name: "mov [0x20], 0xAB (fixture)".into(),
        bytes: vec![0xC6, 0x06, 0x20, 0x00, 0xAB],
        initial: HarteState {
            regs: HarteRegs {
                cs: Some(0x2000),
                ds: Some(0x0000),
                ip: Some(0x0000),
                flags: Some(0x0002),
                ..Default::default()
            },
            ram: vec![
                [0x20000, 0xC6],
                [0x20001, 0x06],
                [0x20002, 0x20],
                [0x20003, 0x00],
                [0x20004, 0xAB],
                [0x00020, 0x00],
            ],
        },
        final_: HarteState {
            regs: HarteRegs {
                ip: Some(0x0005),
                ..Default::default()
            },
            ram: vec![[0x00020, 0xAB]],
        },
    };
    assert_eq!(run_test(&mov_mem, 0xFFFF), TestOutcome::Pass);

    // `add al, 0x01` — opcode 0x04. 0xFF + 0x01 = 0x00: CF+ZF+AF+PF set, SF/OF clear.
    let add_al = HarteTest {
        name: "add al, 1 (fixture)".into(),
        bytes: vec![0x04, 0x01],
        initial: HarteState {
            regs: HarteRegs {
                ax: Some(0x00FF),
                cs: Some(0x3000),
                ip: Some(0x0000),
                flags: Some(0x0002),
                ..Default::default()
            },
            ram: vec![[0x30000, 0x04], [0x30001, 0x01]],
        },
        final_: HarteState {
            regs: HarteRegs {
                ax: Some(0x0000),
                ip: Some(0x0002),
                flags: Some(0x0002 | (1 << 0) | (1 << 6) | (1 << 4) | (1 << 2)),
                ..Default::default()
            },
            ram: vec![],
        },
    };
    assert_eq!(run_test(&add_al, 0xFFFF), TestOutcome::Pass);
}

/// Metadata parse + flag-mask lookup work on the shipped `8088.json` when present.
#[test]
fn metadata_flag_masks() {
    let Some(dir) = harte::corpus_dir() else {
        eprintln!("SKIP harte8088::metadata_flag_masks — corpus not fetched");
        return;
    };
    let meta = harte::load_metadata(&dir);
    // DAA (0x27) leaves OF undefined ⇒ its mask clears bit 11.
    assert_eq!(meta.flags_mask(0x27, None) & (1 << 11), 0, "DAA masks OF");
    // A plain MOV (0x88) has no undefined flags ⇒ mask is all-ones.
    assert_eq!(meta.flags_mask(0x88, None), 0xFFFF);
    // Group 0x80 /1 (OR) leaves AF undefined ⇒ its mask clears bit 4.
    assert_eq!(
        meta.flags_mask(0x80, Some(1)) & (1 << 4),
        0,
        "OR imm8 masks AF"
    );
}
