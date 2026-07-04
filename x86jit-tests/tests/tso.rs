//! Weak-memory ordering (M7-T4, spec §8.2.3/§11): a lock-free **message-passing**
//! litmus that actually races on the host memory model, so the `MemConsistency`
//! tiers are exercised end to end on the JIT — not just emitted.
//!
//! Two guest threads share an array of cells (`data`, `flag`). The writer stores
//! `data = 42` then `flag = 1` per cell; the reader spins until `flag == 1`, then
//! reads `data`. Under x86-TSO (what the guest assumes) the reader can never see
//! `flag == 1` with `data != 42`. On a weak host (ARM) the two writer stores — or
//! the two reader loads — can be observed out of order, so under the `Fast` tier
//! (bare LDR/STR) that violation *can* happen; the `AcqRel`/`FullTso` tiers insert
//! the barriers (§8.2.3) that forbid it.
//!
//! The hard assertion is therefore **`AcqRel` and `FullTso` ⇒ zero violations** —
//! deterministic, and a direct test that the barriers work. `Fast`'s violation
//! count is *reported*, not asserted (it depends on host reordering actually
//! manifesting): on an x86 host every tier is native TSO so all are zero; on an
//! ARM host `Fast` is where reordering can show up. Running this on the ARM CI
//! runner is what gives the tiers real coverage.

use std::sync::{Arc, Barrier};

use iced_x86::code_asm::*;
use x86jit_core::{Exit, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vcpu, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x800_0000; // 128 MiB (holds 4M × 16-byte cells + code)
const WRITER_RIP: u64 = 0x1000;
const READER_RIP: u64 = 0x4000;
const CELLS: u64 = 0x10_0000; // 16-byte cells: data @ +0, flag @ +8
const VIOL: u64 = 0x8000; // reader writes its violation count here
const N: u64 = 4_000_000;

/// `data = 42; flag = 1` for each of `rsi` cells based at `rdi`, then `hlt`.
fn writer_code() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut loop_top = a.create_label();
    let mut done = a.create_label();
    a.xor(rax, rax).unwrap(); // i = 0
    a.mov(rcx, rdi).unwrap(); // cur = &cell[0]
    a.set_label(&mut loop_top).unwrap();
    a.cmp(rax, rsi).unwrap();
    a.jae(done).unwrap();
    a.mov(qword_ptr(rcx), 42i32).unwrap(); // data = 42
    a.mov(qword_ptr(rcx + 8), 1i32).unwrap(); // flag = 1
    a.add(rcx, 16i32).unwrap();
    a.inc(rax).unwrap();
    a.jmp(loop_top).unwrap();
    a.set_label(&mut done).unwrap();
    a.hlt().unwrap();
    a.assemble(WRITER_RIP).unwrap()
}

/// For each cell: spin until `flag == 1`, read `data`; count `data != 42`. Writes
/// the count to `[rdx]`, then `hlt`. `rdi` = cells, `rsi` = count, `rdx` = out.
fn reader_code() -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    let mut loop_top = a.create_label();
    let mut spin = a.create_label();
    let mut ok = a.create_label();
    let mut done = a.create_label();
    a.xor(rax, rax).unwrap(); // i = 0
    a.xor(r8, r8).unwrap(); // violations = 0
    a.mov(rcx, rdi).unwrap(); // cur = &cell[0]
    a.set_label(&mut loop_top).unwrap();
    a.cmp(rax, rsi).unwrap();
    a.jae(done).unwrap();
    a.set_label(&mut spin).unwrap();
    a.mov(r9, qword_ptr(rcx + 8)).unwrap(); // flag
    a.test(r9, r9).unwrap();
    a.jz(spin).unwrap();
    a.mov(r10, qword_ptr(rcx)).unwrap(); // data (after seeing flag)
    a.cmp(r10, 42i32).unwrap();
    a.je(ok).unwrap();
    a.inc(r8).unwrap(); // saw flag but stale data → reorder
    a.set_label(&mut ok).unwrap();
    a.add(rcx, 16i32).unwrap();
    a.inc(rax).unwrap();
    a.jmp(loop_top).unwrap();
    a.set_label(&mut done).unwrap();
    a.mov(qword_ptr(rdx), r8).unwrap();
    a.hlt().unwrap();
    a.assemble(READER_RIP).unwrap()
}

/// Run one vcpu to `hlt`. The barrier releases both threads at the same instant
/// so writer and reader march in lockstep — the reader keeps catching the write
/// wavefront (`flag` flipping 0→1 under it), which is where a weak host's reorder
/// becomes observable. If the writer ran ahead, every cell would already be fully
/// published and no reorder could show.
fn run_to_hlt(vm: &Arc<Vm>, mut cpu: Vcpu, start: &Barrier) {
    start.wait();
    loop {
        match cpu.run(vm, None) {
            Exit::Hlt => break,
            Exit::BudgetExhausted => continue,
            other => panic!("unexpected exit at rip={:#x}: {other:?}", cpu.reg(Reg::Rip)),
        }
    }
}

/// Race the writer and reader under `tier`; return how many cells the reader saw
/// with `flag == 1` but stale `data`.
fn message_passing_violations(tier: MemConsistency) -> u64 {
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: tier,
        },
        Box::new(JitBackend::new()),
    );
    vm.map(0, FLAT as usize, Prot::RW, RegionKind::Ram).unwrap();
    for (addr, code) in [(WRITER_RIP, writer_code()), (READER_RIP, reader_code())] {
        for (i, b) in code.iter().enumerate() {
            vm.mem.write(addr + i as u64, *b as u64, 1).unwrap();
        }
    }

    let mut writer = vm.new_vcpu();
    writer.set_reg(Reg::Rip, WRITER_RIP);
    writer.set_reg(Reg::Rdi, CELLS);
    writer.set_reg(Reg::Rsi, N);

    let mut reader = vm.new_vcpu();
    reader.set_reg(Reg::Rip, READER_RIP);
    reader.set_reg(Reg::Rdi, CELLS);
    reader.set_reg(Reg::Rsi, N);
    reader.set_reg(Reg::Rdx, VIOL);

    let vm = Arc::new(vm);
    let start = Arc::new(Barrier::new(2));
    let (vm_w, st_w) = (Arc::clone(&vm), Arc::clone(&start));
    let h = std::thread::spawn(move || run_to_hlt(&vm_w, writer, &st_w));
    run_to_hlt(&vm, reader, &start);
    h.join().unwrap();

    vm.mem.read(VIOL, 8).unwrap()
}

#[test]
fn acqrel_and_fulltso_forbid_reorder_fast_reveals_it() {
    // The proof: the barriers must eliminate every reorder the guest can't tolerate.
    let acqrel = message_passing_violations(MemConsistency::AcqRel);
    assert_eq!(
        acqrel, 0,
        "AcqRel must restore x86-TSO — no reorders may be observed"
    );

    let fulltso = message_passing_violations(MemConsistency::FullTso);
    assert_eq!(
        fulltso, 0,
        "FullTso is sequentially consistent — certainly no reorders"
    );

    // Evidence, not an assertion: on a weak (ARM) host this is where reorders show
    // up; on an x86 host it's native TSO, so it's zero too. Either way it must not
    // exceed the total cell count.
    let fast = message_passing_violations(MemConsistency::Fast);
    assert!(fast <= N, "sanity: violations bounded by cell count");
    eprintln!(
        "message-passing reorders — Fast: {fast}, AcqRel: {acqrel}, FullTso: {fulltso} \
         (Fast > 0 only on a weak-memory host)"
    );
}
