//! SingleStepTests **80286** corpus as the authoritative Real16 CPU oracle.
//!
//! Replays each fetched per-opcode MOO file on the x86jit interpreter in
//! `CpuMode::Real16` and reports how the architecturally defined final state — and
//! in-guest exception delivery — compares against real-80286-captured truth. The 286
//! is our target CPU, so a divergence here is a genuine model bug (no generation
//! excuse). See [`x86jit_tests::ss286`] for the loader, the terminating-`HALT`
//! protocol, and the flag-masking policy.
//!
//! # Running
//! The corpus is large and gitignored. Fetch it first:
//! ```text
//! x86jit-tests/vendor/80286/fetch.sh            # full real-mode corpus
//! x86jit-tests/vendor/80286/fetch.sh 00 F7.6    # a subset
//! ```
//! With the corpus absent, [`corpus_runs`] **skips with a message** and
//! [`fixtures_exercise_loader`] still runs (hand-written cases in the corpus shape).
//!
//! `SS286_LIMIT=N` caps tests-per-opcode for a faster sweep; the CI/full run leaves it
//! unset. `SS286_VERBOSE=1` prints per-opcode lines.

use x86jit_tests::ss286::{
    self, run_test, Runner, Ss286Exception, Ss286Regs, Ss286State, Ss286Test, Summary, TestOutcome,
};

/// Per-opcode cap, from `SS286_LIMIT` (0/unset = all tests in the file).
fn limit() -> usize {
    std::env::var("SS286_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[test]
fn corpus_runs() {
    let Some(dir) = ss286::corpus_dir() else {
        eprintln!(
            "SKIP ss286::corpus_runs — corpus not fetched.\n\
             Run x86jit-tests/vendor/80286/fetch.sh to pull it (gitignored).\n\
             CI must fetch the corpus before this test is meaningful."
        );
        return;
    };

    let meta = ss286::load_metadata(&dir);
    let revoked = ss286::load_revocations(&dir);
    let files = ss286::opcode_files(&dir);
    assert!(
        !files.is_empty(),
        "corpus dir present but holds no opcode files"
    );

    let cap = limit();
    let verbose = std::env::var_os("SS286_VERBOSE").is_some();
    let mut summary = Summary::default();
    // One reusable 16 MB Real16 VM for the whole sweep (see `Runner`).
    let mut runner = Runner::new();

    for (stem, path) in &files {
        let Some((opcode, reg)) = ss286::parse_stem(stem) else {
            continue;
        };
        let mask = meta.flags_mask(opcode, reg);
        let tests = ss286::load_tests(path, cap);
        for t in &tests {
            if revoked.contains(&t.hash) {
                continue;
            }
            let outcome = runner.run(t, mask);
            summary.record(stem, t, outcome);
        }
        if verbose {
            let t = &summary.by_op[stem];
            eprintln!(
                "  {stem}: pass {} fail {} trapped {} unsupported {} (exc {}/{})",
                t.pass, t.fail, t.trapped, t.unsupported, t.exc_pass, t.exc_total
            );
        }
    }

    print_summary(&summary);

    // This test is a **reporting oracle + regression tripwire**, not a 100%-conformance
    // gate. The corpus surfaces a real gap list — unlifted opcodes (`unsupported`) and
    // genuine divergences (`fail`/`trapped`) — that drives follow-up lift work; forcing
    // them all green would hide exactly what we want to see. So we do NOT assert a high
    // pass rate.
    //
    // We DO assert a floor on the *decided* tests (pass + fail + trapped; excludes
    // opcodes not lifted at all) so a change that badly regresses the interpreter turns
    // this red. The floor is slack enough never to flake on the existing gap list yet
    // catch a real collapse. The full gap list prints above regardless.
    let tot = summary.totals();
    let decided = tot.decided();
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

/// Human-readable summary: overall + decided pass rate, exception-delivery coverage,
/// then a per-opcode gap list of failing / trapped / unsupported opcodes (the follow-up
/// lift work).
fn print_summary(s: &Summary) {
    let tot = s.totals();
    let total = tot.total();
    eprintln!("\n=== SingleStepTests 80286 Real16 oracle — summary ===");
    eprintln!(
        "total {total}  pass {}  fail {}  trapped {}  unsupported {}  port_io {}",
        tot.pass, tot.fail, tot.trapped, tot.unsupported, tot.port_io
    );
    if total > 0 {
        let decided = tot.decided();
        eprintln!(
            "overall pass rate: {:.4}   decided pass rate: {:.4}  \
             (decided = pass+fail+trapped; excludes unsupported + port_io)",
            tot.pass as f64 / total as f64,
            if decided == 0 {
                1.0
            } else {
                tot.pass as f64 / decided as f64
            },
        );
    }
    if tot.exc_total > 0 {
        eprintln!(
            "exception delivery: {}/{} passed ({:.4}) — vector + pushed CS:IP:FLAGS frame validated",
            tot.exc_pass,
            tot.exc_total,
            tot.exc_pass as f64 / tot.exc_total as f64,
        );
        // Architectural exception vectors (0-31) individually — these are the real 286
        // faults/traps and the interesting gaps. Software `int n` (32-255) hits random
        // handler vectors and is aggregated into one bucket.
        eprint!("  by vector (pass/total): ");
        for (v, (pass, total)) in s.exc_by_vec.range(0u8..32) {
            eprint!("#{v}={pass}/{total} ");
        }
        let (sw_pass, sw_total) = s
            .exc_by_vec
            .range(32u8..=255)
            .fold((0u64, 0u64), |(p, t), (_, (vp, vt))| (p + vp, t + vt));
        if sw_total > 0 {
            eprint!("| int-n(#32-255)={sw_pass}/{sw_total}");
        }
        eprintln!();
    }

    let mut gaps: Vec<(&String, &ss286::OpTally)> = s
        .by_op
        .iter()
        .filter(|(_, t)| t.fail > 0 || t.trapped > 0 || t.unsupported > 0)
        .collect();
    gaps.sort_by_key(|(stem, _)| (*stem).clone());
    if !gaps.is_empty() {
        eprintln!("\n--- gap list (opcode: pass/fail/trapped/unsupported/port_io of total) ---");
        for (stem, t) in gaps {
            eprint!(
                "  {stem}: {}/{}/{}/{}/{} of {}",
                t.pass,
                t.fail,
                t.trapped,
                t.unsupported,
                t.port_io,
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
/// exact corpus shape, so the parse → Real16-run → compare path is always tested. Each
/// fixture includes the terminating `HALT` (0xF4) the real corpus appends.
#[test]
fn fixtures_exercise_loader() {
    // `inc ax` (0x40) at CS:IP = 0x1000:0x0000 ⇒ linear 0x10000, then the terminating
    // HALT. 0x00FF+1 = 0x0100; INC leaves CF untouched, sets ZF/SF/PF/AF/OF from the
    // result. Final IP = 0x0002 (past the INC *and* the HALT).
    let inc_ax = Ss286Test {
        idx: 0,
        name: "inc ax (fixture)".into(),
        bytes: vec![0x40, 0xF4],
        initial: Ss286State {
            regs: Ss286Regs {
                ax: Some(0x00FF),
                cs: Some(0x1000),
                ip: Some(0x0000),
                flags: Some(0x0002),
                ..Default::default()
            },
            ram: vec![(0x10000, 0x40), (0x10001, 0xF4)],
        },
        final_: Ss286State {
            regs: Ss286Regs {
                ax: Some(0x0100),
                ip: Some(0x0002),
                // AF set (nibble carry), PF set (0x00 low byte), CF untouched.
                flags: Some(0x0002 | (1 << 4) | (1 << 2)),
                ..Default::default()
            },
            ram: vec![],
        },
        exception: None,
        hash: String::new(),
    };
    assert_eq!(run_test(&inc_ax, 0xFFFF), TestOutcome::Pass);

    // `mov byte [ds:0x0020], 0xAB` — C6 06 20 00 AB, then HALT. DS=0 ⇒ linear 0x20.
    // Writes memory, touches no flags. Final IP = 0x0006 (5-byte mov + HALT).
    let mov_mem = Ss286Test {
        idx: 1,
        name: "mov [0x20], 0xAB (fixture)".into(),
        bytes: vec![0xC6, 0x06, 0x20, 0x00, 0xAB, 0xF4],
        initial: Ss286State {
            regs: Ss286Regs {
                cs: Some(0x2000),
                ds: Some(0x0000),
                ip: Some(0x0000),
                flags: Some(0x0002),
                ..Default::default()
            },
            ram: vec![
                (0x20000, 0xC6),
                (0x20001, 0x06),
                (0x20002, 0x20),
                (0x20003, 0x00),
                (0x20004, 0xAB),
                (0x20005, 0xF4),
                (0x00020, 0x00),
            ],
        },
        final_: Ss286State {
            regs: Ss286Regs {
                ip: Some(0x0006),
                ..Default::default()
            },
            ram: vec![(0x00020, 0xAB)],
        },
        exception: None,
        hash: String::new(),
    };
    assert_eq!(run_test(&mov_mem, 0xFFFF), TestOutcome::Pass);

    // `add al, 0x01` — 04 01, then HALT. 0xFF+0x01 = 0x00: CF+ZF+AF+PF set, SF/OF clear.
    // Final IP = 0x0003 (2-byte add + HALT).
    let add_al = Ss286Test {
        idx: 2,
        name: "add al, 1 (fixture)".into(),
        bytes: vec![0x04, 0x01, 0xF4],
        initial: Ss286State {
            regs: Ss286Regs {
                ax: Some(0x00FF),
                cs: Some(0x3000),
                ip: Some(0x0000),
                flags: Some(0x0002),
                ..Default::default()
            },
            ram: vec![(0x30000, 0x04), (0x30001, 0x01), (0x30002, 0xF4)],
        },
        final_: Ss286State {
            regs: Ss286Regs {
                ax: Some(0x0000),
                ip: Some(0x0003),
                flags: Some(0x0002 | (1 << 0) | (1 << 6) | (1 << 4) | (1 << 2)),
                ..Default::default()
            },
            ram: vec![],
        },
        exception: None,
        hash: String::new(),
    };
    assert_eq!(run_test(&add_al, 0xFFFF), TestOutcome::Pass);
}

/// Exercise the exception-delivery path of the loader end-to-end without the corpus:
/// `int3` (0xCC) must vector in-guest through IVT[3], push the FLAGS:CS:IP frame, and
/// run into the HALT injected at the handler entry — with the pushed FLAGS word
/// flag-masked exactly as the real corpus's exception tests are.
#[test]
fn fixture_exception_delivery() {
    // int3 at CS:IP = 0x2000:0x0000 ⇒ linear 0x20000. SS=0x3000 (base 0x30000),
    // SP=0x0100. FLAGS = only the reserved bit set.
    // IVT[3] at linear 0x0C: handler = 0x4000:0x0500 ⇒ linear 0x40500, seeded with HALT.
    // int3 pushes FLAGS, CS, then the return IP (0x0001, the byte after int3):
    //   FLAGS -> [0x300FE] (sp 0x00FE)   <- flag_address
    //   CS    -> [0x300FC] (sp 0x00FC)
    //   IP    -> [0x300FA] (sp 0x00FA)
    // Final: CS:IP = 0x4000:0x0501 (handler entry + the injected HALT), SP = 0x00FA.
    let int3 = Ss286Test {
        idx: 0,
        name: "int3 (fixture)".into(),
        bytes: vec![0xCC, 0xF4],
        initial: Ss286State {
            regs: Ss286Regs {
                cs: Some(0x2000),
                ss: Some(0x3000),
                sp: Some(0x0100),
                ip: Some(0x0000),
                flags: Some(0x0002),
                ..Default::default()
            },
            ram: vec![
                (0x20000, 0xCC), // int3
                (0x20001, 0xF4), // (unused terminating HALT — int3 diverts before it)
                // IVT[3] = 0x4000:0x0500
                (0x0000C, 0x00),
                (0x0000D, 0x05),
                (0x0000E, 0x00),
                (0x0000F, 0x40),
                (0x40500, 0xF4), // injected HALT at handler entry
                // stack cells int3 will overwrite (seed so they are "touched"/zeroed)
                (0x300FA, 0x00),
                (0x300FB, 0x00),
                (0x300FC, 0x00),
                (0x300FD, 0x00),
                (0x300FE, 0x00),
                (0x300FF, 0x00),
            ],
        },
        final_: Ss286State {
            regs: Ss286Regs {
                cs: Some(0x4000),
                ip: Some(0x0501),
                sp: Some(0x00FA),
                flags: Some(0x0002),
                ..Default::default()
            },
            ram: vec![
                (0x300FA, 0x01), // return IP low
                (0x300FB, 0x00), // return IP high
                (0x300FC, 0x00), // CS low
                (0x300FD, 0x20), // CS high
                (0x300FE, 0x02), // FLAGS low (flag_address)
                (0x300FF, 0x00), // FLAGS high
            ],
        },
        exception: Some(Ss286Exception {
            number: 3,
            flag_address: 0x300FE,
        }),
        hash: String::new(),
    };
    // A lifter that does not yet vector int3 in real mode would report Unsupported; the
    // point of this fixture is that when it IS delivered, the loader validates the frame.
    let outcome = run_test(&int3, 0xFFFF);
    assert!(
        matches!(outcome, TestOutcome::Pass | TestOutcome::Unsupported),
        "int3 fixture: {outcome:?}"
    );
}

/// Metadata parse + flag-mask lookup work on the shipped `metadata.json` when present.
#[test]
fn metadata_flag_masks() {
    let Some(dir) = ss286::corpus_dir() else {
        eprintln!("SKIP ss286::metadata_flag_masks — corpus not fetched");
        return;
    };
    let meta = ss286::load_metadata(&dir);
    // DAA (0x27) leaves OF undefined ⇒ its mask clears bit 11.
    assert_eq!(meta.flags_mask(0x27, None) & (1 << 11), 0, "DAA masks OF");
    // A plain MOV (0x88) has no undefined flags ⇒ mask is all-ones.
    assert_eq!(meta.flags_mask(0x88, None), 0xFFFF);
    // Group 0x80 /1 (OR imm8) leaves AF undefined ⇒ its mask clears bit 4.
    assert_eq!(
        meta.flags_mask(0x80, Some(1)) & (1 << 4),
        0,
        "OR imm8 masks AF"
    );
    // MUL (0xF7 /4) leaves SF/ZF/AF/PF undefined ⇒ its mask clears those bits.
    let mul_mask = meta.flags_mask(0xF7, Some(4));
    assert_eq!(mul_mask & (1 << 6), 0, "MUL masks ZF");
    assert_eq!(mul_mask & (1 << 7), 0, "MUL masks SF");
}
