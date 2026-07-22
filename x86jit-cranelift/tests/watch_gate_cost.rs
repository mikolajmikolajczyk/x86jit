//! task-283 AC#5: what does the watched-store gate actually cost?
//!
//! The Cranelift store gate is keyed on a process-wide `watch_count`, not on the
//! address, so watching one page anywhere turns EVERY store out of compiled code into
//! a call into Rust that almost always discovers the page is not watched. An embedder
//! measured 388M such calls in a 10 s window — but the per-call cost is evidently
//! small (well-predicted branch, hot L1, immediate return), and the product was never
//! measured. This prices it before anyone rewrites the gate.
//!
//! Run with:
//!   cargo test -p x86jit-cranelift --release --test watch_gate_cost -- --ignored --nocapture

use std::time::Instant;

use x86jit_core::{Exit, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const RAM: u64 = 0x100000;
const ENTRY: u64 = 0x1000;
const DATA: u64 = 0x40000; // where the loop stores
const ELSEWHERE: u64 = 0x80000; // an unrelated page, never stored to
const ITERS: u64 = 20_000_000;

/// `L: mov [rdi], eax ; dec ecx ; jnz L ; hlt` — a store-dominated loop.
const STORE_LOOP: &[u8] = &[
    0x89, 0x07, // mov [rdi], eax
    0xFF, 0xC9, // dec ecx
    0x75, 0xFA, // jnz L
    0xF4, // hlt
];

/// Run the loop with `watch` applied, returning (wall ns, helper calls).
fn run(watch: Option<(u64, u64)>) -> (u128, u64) {
    let jit = JitBackend::new();
    let mut vm = Vm::with_backend(VmConfig::flat(RAM), Box::new(jit));
    vm.set_tier_up_after(Some(0)); // compile after the first execution
    vm.map(0, RAM as usize, Prot::RWX, RegionKind::Ram).unwrap();
    vm.write_bytes(ENTRY, STORE_LOOP).unwrap();
    if let Some((addr, len)) = watch {
        vm.watch_range(addr, len);
    }

    let once = |vm: &Vm, n: u64| {
        let mut cpu = vm.new_vcpu();
        cpu.set_reg(Reg::Rip, ENTRY);
        cpu.set_reg(Reg::Rdi, DATA);
        cpu.set_reg(Reg::Rax, 0xdead_beef);
        cpu.set_reg(Reg::Rcx, n);
        assert!(matches!(cpu.run(vm, None), Exit::Hlt));
    };

    once(&vm, 1_000); // warm up + compile
    let base = vm
        .backend
        .helper_calls()
        .iter()
        .map(|(_, n)| n)
        .sum::<u64>();
    let t = Instant::now();
    once(&vm, ITERS);
    let ns = t.elapsed().as_nanos();
    let calls = vm
        .backend
        .helper_calls()
        .iter()
        .map(|(_, n)| n)
        .sum::<u64>()
        - base;
    (ns, calls)
}

#[test]
#[ignore = "timing measurement, not a correctness gate — run explicitly"]
fn price_the_watched_store_gate() {
    let cases = [
        ("nothing watched", None),
        ("one UNRELATED page watched", Some((ELSEWHERE, 0x1000))),
        ("the stored-to page watched", Some((DATA, 0x1000))),
    ];
    println!(
        "\n{:<30}{:>12}{:>14}{:>16}{:>14}",
        "case", "ns/store", "helper calls", "calls/store", "vs unwatched"
    );
    let mut baseline = 0.0f64;
    for (i, (name, watch)) in cases.iter().enumerate() {
        // Best of 3: this is a timing measurement on a shared machine.
        let (mut best_ns, mut calls) = (u128::MAX, 0);
        for _ in 0..3 {
            let (ns, c) = run(*watch);
            if ns < best_ns {
                best_ns = ns;
                calls = c;
            }
        }
        let per = best_ns as f64 / ITERS as f64;
        if i == 0 {
            baseline = per;
        }
        println!(
            "{:<30}{:>12.3}{:>14}{:>16.2}{:>13.1}%",
            name,
            per,
            calls,
            calls as f64 / ITERS as f64,
            (per / baseline - 1.0) * 100.0
        );
    }
    println!(
        "\nThe middle row is what task-283 removes: the store's page is NOT watched, \
         but the process-wide gate calls out anyway.\n"
    );
}
