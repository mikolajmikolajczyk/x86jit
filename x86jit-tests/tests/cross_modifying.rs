//! Cross-modifying code across two vcpus (task-323 AC#6, SDM Vol 3A §11.1.3).
//!
//! One vcpu writes into a code page another vcpu is about to execute. The SDM calls the
//! unsynchronized form **model-specific** — "IA-32 processors exhibit model-specific
//! behavior when executing cross-modifying code, depending upon how far ahead of the
//! executing processors current execution pointer the code has been modified" — and
//! specifies the protocol that makes it defined, putting work on BOTH sides:
//!
//! ```text
//! (* Action of Modifying Processor *)
//! Memory_Flag := 0;
//! Store modified code (as data) into code segment;
//! Memory_Flag := 1;
//!
//! (* Action of Executing Processor *)
//! WHILE (Memory_Flag ≠ 1) Wait for code to update; ELIHW;
//! Execute serializing instruction; (* For example, CPUID instruction *)
//! Begin executing modified code;
//! ```
//!
//! **This is what replaced the original acceptance criterion**, which asked for a
//! deterministic test pausing between a link slot's load and the transfer through it.
//! Two things were wrong with that. The pause is inside *generated* code, so forcing it
//! needs a hook in emitted code that does not exist. And the property it demanded — a
//! stale translation can never run — is stronger than the architecture grants: without
//! the executing processor's serializing instruction, real silicon may run stale bytes
//! too. Holding an emulator to more than the ISA promises, at the cost of a check on
//! every chain transfer, buys nothing a guest can rely on.
//!
//! What a guest CAN rely on is the protocol above, so that is what this pins. It is
//! deterministic by construction rather than by luck: the flag handshake *is* the
//! synchronization, so there is no interleaving to win.
//!
//! Why it passes here has nothing to do with the serializing instruction, which this
//! engine treats as an ordinary op: a compiled store to a code page marks it dirty
//! (task-329), and the compiled inner loop leaves its chain as soon as any code page is
//! dirty, so the polling vcpu reaches `Vm::handle_smc` before it re-enters the target.
//!
//! Both halves are load-bearing, checked by breaking each in turn: with the store's
//! code-page gate stubbed out, and with the chain-leave removed, the JIT case fails.
//! The interpreter case survives the second — it reaches `handle_smc` every block by
//! construction — which is exactly why running this on one backend would have proved
//! nothing about the other.

use std::sync::Arc;
use std::thread;

use iced_x86::code_asm::*;
use x86jit_core::{Backend, Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

const FLAT: u64 = 0x2_0000;
const MODIFIER: u64 = 0x1000;
const EXECUTOR: u64 = 0x2000;
/// The code being rewritten, on its own page so neither driver's page is invalidated.
const TARGET: u64 = 0x3000;
/// `Memory_Flag`: the modifier raises it once the new bytes are in place.
const MODIFIED: u64 = 0x4000;
/// The executor raises this once it has run `TARGET` at least once, so the modification
/// lands on a *translated* block. Without it the executor might lift the new bytes on
/// its first visit and the test would prove nothing.
const READY: u64 = 0x4008;
const STACK_MOD: u64 = 0x9000;
const STACK_EXE: u64 = 0xA000;

/// Bounded so a failure to converge fails the test instead of hanging CI.
const MAX_SLICES: usize = 20_000;
const SLICE_BLOCKS: u64 = 64;

fn assemble(origin: u64, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(64).unwrap();
    build(&mut a);
    a.assemble(origin).unwrap()
}

/// Run one vcpu to `hlt` in bounded slices. Returns `None` if it never halted.
fn run_bounded(vm: &Arc<Vm>, entry: u64, stack: u64) -> Option<u64> {
    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, stack);
    for _ in 0..MAX_SLICES {
        match cpu.run(vm, Some(SLICE_BLOCKS)) {
            Exit::Hlt => return Some(cpu.reg(Reg::Rax)),
            Exit::BudgetExhausted => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    None
}

fn cross_modifying_protocol(mk: fn() -> Box<dyn Backend>) {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), mk());
    vm.map(0, FLAT as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();

    // The code under modification: `mov eax, 1; ret`, whose imm32 becomes 2.
    let target = assemble(TARGET, |a| {
        a.mov(eax, 1i32).unwrap();
        a.ret().unwrap();
    });
    vm.write_bytes(TARGET, &target).unwrap();

    // Modifying processor: wait for the target to be translated, store the new code,
    // THEN raise Memory_Flag. The store order is the protocol's; x86 store-store
    // ordering is what makes the flag mean "the bytes are there".
    let modifier = assemble(MODIFIER, |a| {
        let mut wait = a.create_label();
        a.mov(r13, READY).unwrap();
        a.set_label(&mut wait).unwrap();
        a.mov(ecx, dword_ptr(r13)).unwrap();
        a.cmp(ecx, 1i32).unwrap();
        a.jne(wait).unwrap();

        a.mov(r15, TARGET).unwrap();
        a.mov(byte_ptr(r15), 0xB8i32).unwrap();
        a.mov(dword_ptr(r15 + 1), 2i32).unwrap();

        a.mov(r14, MODIFIED).unwrap();
        a.mov(dword_ptr(r14), 1i32).unwrap();
        a.hlt().unwrap();
    });
    vm.write_bytes(MODIFIER, &modifier).unwrap();

    // Executing processor: run the target once (so a translation exists), announce
    // readiness, then follow the SDM loop — poll, serialize, execute.
    let executor = assemble(EXECUTOR, |a| {
        let mut poll = a.create_label();
        a.mov(r15, TARGET).unwrap();
        a.call(r15).unwrap(); // caches + marks the page; eax = 1

        a.mov(r13, READY).unwrap();
        a.mov(dword_ptr(r13), 1i32).unwrap();

        a.mov(r14, MODIFIED).unwrap();
        a.set_label(&mut poll).unwrap();
        a.mov(ecx, dword_ptr(r14)).unwrap();
        a.cmp(ecx, 1i32).unwrap();
        a.jne(poll).unwrap();

        // The serializing instruction the protocol requires. `cpuid` clobbers
        // eax/ebx/ecx/edx, so it goes BEFORE the call that produces the result.
        a.mov(eax, 0i32).unwrap();
        a.cpuid().unwrap();

        a.call(r15).unwrap(); // must observe eax = 2
        a.hlt().unwrap();
    });
    vm.write_bytes(EXECUTOR, &executor).unwrap();

    let vm = Arc::new(vm);
    let m = {
        let vm = Arc::clone(&vm);
        thread::spawn(move || run_bounded(&vm, MODIFIER, STACK_MOD))
    };
    let e = {
        let vm = Arc::clone(&vm);
        thread::spawn(move || run_bounded(&vm, EXECUTOR, STACK_EXE))
    };

    let modified = m.join().unwrap();
    let executed = e.join().unwrap();
    assert!(modified.is_some(), "the modifying vcpu never halted");
    let result = executed.expect("the executing vcpu never halted");
    assert_eq!(
        result as u32, 2,
        "after the SDM cross-modifying handshake the executor ran the STALE translation"
    );
}

#[test]
fn cross_modifying_protocol_interp() {
    cross_modifying_protocol(|| Box::new(InterpreterBackend));
}

#[test]
fn cross_modifying_protocol_jit() {
    cross_modifying_protocol(|| Box::new(JitBackend::new()));
}
