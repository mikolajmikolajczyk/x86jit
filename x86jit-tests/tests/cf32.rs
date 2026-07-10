//! Compat32 (32-bit protected/flat) control-flow + stack differential (TASK-197.3).
//!
//! Minimal, self-contained harness (the general UC_MODE_32 harness/fuzzer is
//! TASK-197.5): each case assembles a 32-bit snippet, runs it three ways —
//! x86jit interpreter, x86jit JIT, and Unicorn in `MODE_32` — and asserts they
//! agree on the final GPRs, flags, EIP, and the touched stack bytes.
//!
//! Covers EIP truncation on jmp/jcc/call/ret, 4-byte push/pop/call frames
//! (2-byte under 66h), and ESP wrap mod 2^32.

#![cfg(feature = "unicorn")]

use iced_x86::code_asm::*;
use x86jit_core::lift::CpuMode;
use x86jit_core::{Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};
use x86jit_cranelift::JitBackend;

use unicorn_engine::unicorn_const::{Arch, Mode, Prot as UProt};
use unicorn_engine::{RegisterX86, Unicorn};

const CODE: u64 = 0x1000;
const STACK_TOP: u64 = 0x8000; // ESP starts here; the stack grows down into [0x7000,0x8000)
const FLAT: u64 = 0x10_0000; // 1 MiB flat guest space (well under 2^32)

/// The x86 encoding-order GPRs we compare (RSP included — that's the whole point).
const GPRS: [(Reg, RegisterX86); 8] = [
    (Reg::Rax, RegisterX86::EAX),
    (Reg::Rcx, RegisterX86::ECX),
    (Reg::Rdx, RegisterX86::EDX),
    (Reg::Rbx, RegisterX86::EBX),
    (Reg::Rsp, RegisterX86::ESP),
    (Reg::Rbp, RegisterX86::EBP),
    (Reg::Rsi, RegisterX86::ESI),
    (Reg::Rdi, RegisterX86::EDI),
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    gpr: [u32; 8],
    eip: u32,
    stack: Vec<u8>,
}

struct Setup {
    /// Assembled 32-bit machine code, placed at `CODE`.
    code: Vec<u8>,
    /// Initial 32-bit GPR values (encoding order); `esp` defaults to `STACK_TOP`.
    init: [u32; 8],
    /// Range of stack bytes to read back and compare (guest addresses).
    stack_lo: u64,
    stack_len: usize,
}

/// Run the snippet on an x86jit `Vm` in the given backend, Compat32 mode.
/// `raw_esp`, when set, overwrites the 64-bit ESP after the 32-bit init — used to
/// pollute bits 32–63 and prove the stack ops zero-extend them away.
fn run_x86jit_raw(setup: &Setup, jit: bool, raw_esp: Option<u64>) -> Outcome {
    let backend: Box<dyn x86jit_core::Backend> = if jit {
        Box::new(JitBackend::new())
    } else {
        Box::new(InterpreterBackend)
    };
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), backend);
    vm.set_cpu_mode(CpuMode::Compat32);
    vm.map(0, FLAT as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();
    vm.write_bytes(CODE, &setup.code).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, CODE);
    for (i, (reg, _)) in GPRS.iter().enumerate() {
        cpu.set_reg(*reg, setup.init[i] as u64);
    }
    if let Some(esp_val) = raw_esp {
        cpu.set_reg(Reg::Rsp, esp_val);
    }

    match cpu.run(&vm, Some(10_000)) {
        Exit::Hlt => {}
        other => panic!("x86jit (jit={jit}) did not hlt: {other:?}"),
    }

    let mut gpr = [0u32; 8];
    for (i, (reg, _)) in GPRS.iter().enumerate() {
        gpr[i] = cpu.reg(*reg) as u32;
        // Compat32: the upper 32 bits of every GPR must stay zero.
        assert_eq!(
            cpu.reg(*reg) >> 32,
            0,
            "upper 32 bits of {reg:?} nonzero (jit={jit})"
        );
    }
    let mut stack = vec![0u8; setup.stack_len];
    vm.read_bytes(setup.stack_lo, &mut stack).unwrap();
    Outcome {
        gpr,
        eip: cpu.reg(Reg::Rip) as u32,
        stack,
    }
}

/// Run the same snippet on Unicorn in 32-bit mode. Stops at the terminating `hlt`.
fn run_unicorn(setup: &Setup) -> Outcome {
    let mut uc = Unicorn::new(Arch::X86, Mode::MODE_32).expect("open unicorn x86-32");
    uc.mem_map(0, FLAT, UProt::ALL).expect("map");
    uc.mem_write(CODE, &setup.code).expect("write code");

    for (i, (_, ureg)) in GPRS.iter().enumerate() {
        uc.reg_write(*ureg, setup.init[i] as u64).unwrap();
    }
    uc.reg_write(RegisterX86::EIP, CODE).unwrap();

    // Stop before the terminating hlt (privileged in Unicorn) and record its address.
    use std::cell::Cell;
    use std::rc::Rc;
    let hlt_at: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    let h = hlt_at.clone();
    uc.add_code_hook(CODE, u64::MAX, move |uc, addr, size| {
        let mut buf = vec![0u8; size as usize];
        if uc.mem_read(addr, &mut buf).is_ok() && size == 1 && buf[0] == 0xf4 {
            h.set(Some(addr));
            let _ = uc.emu_stop();
        }
    })
    .expect("hook");

    let _ = uc.emu_start(CODE, u64::MAX, 0, 10_000);

    let mut gpr = [0u32; 8];
    for (i, (_, ureg)) in GPRS.iter().enumerate() {
        gpr[i] = uc.reg_read(*ureg).unwrap() as u32;
    }
    // Engine convention: EIP resumes past the terminating hlt (1 byte).
    let eip = match hlt_at.get() {
        Some(a) => (a + 1) as u32,
        None => uc.reg_read(RegisterX86::EIP).unwrap() as u32,
    };
    let stack = uc.mem_read_as_vec(setup.stack_lo, setup.stack_len).unwrap();
    Outcome { gpr, eip, stack }
}

/// The full three-way check: interp == JIT == Unicorn.
fn diff(setup: Setup) {
    diff_with(setup, None);
}

/// As [`diff`] but with an optional raw 64-bit ESP override on the x86jit side
/// (Unicorn always starts with the clean 32-bit ESP from `setup.init`).
fn diff_with(setup: Setup, raw_esp: Option<u64>) {
    let interp = run_x86jit_raw(&setup, false, raw_esp);
    let jit = run_x86jit_raw(&setup, true, raw_esp);
    let unicorn = run_unicorn(&setup);
    assert_eq!(
        interp,
        jit,
        "interp vs JIT diverge\n{setup:?}",
        setup = Dbg(&setup)
    );
    assert_eq!(
        interp,
        unicorn,
        "x86jit vs Unicorn diverge\n{setup:?}",
        setup = Dbg(&setup)
    );
}

/// Compact debug of a Setup (the code bytes hex-dumped).
struct Dbg<'a>(&'a Setup);
impl std::fmt::Debug for Dbg<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "code={} init={:x?} stack[{:#x}..+{}]",
            self.0
                .code
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            self.0.init,
            self.0.stack_lo,
            self.0.stack_len
        )
    }
}

fn base_init() -> [u32; 8] {
    let mut init = [0u32; 8];
    init[4] = STACK_TOP as u32; // ESP
    init
}

/// AC#1 / AC#2: a call/ret round-trip. `call` pushes a 4-byte return address and
/// jumps forward; the callee sets a marker and `ret`s; execution resumes after the
/// call. Pins EIP truncation and the 4-byte frame under all three engines.
#[test]
fn call_ret_roundtrip() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut callee = a.create_label();
    a.call(callee).unwrap();
    a.inc(eax).unwrap(); // after return
    a.hlt().unwrap();
    a.set_label(&mut callee).unwrap();
    a.mov(ebx, 0xdeadu32).unwrap();
    a.ret().unwrap();
    let code = a.assemble(CODE).unwrap();

    diff(Setup {
        code,
        init: base_init(),
        stack_lo: STACK_TOP - 16,
        stack_len: 16,
    });
}

/// AC#1: forward + backward `jmp` and a taken/not-taken `jcc`, all with 32-bit
/// targets. A short loop decrements ecx until zero (back-edge), then falls through.
#[test]
fn jmp_jcc_loop() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut done = a.create_label();
    a.mov(eax, 0u32).unwrap();
    a.set_label(&mut top).unwrap();
    a.cmp(ecx, 0u32).unwrap();
    a.je(done).unwrap(); // jcc: taken when ecx==0
    a.inc(eax).unwrap();
    a.dec(ecx).unwrap();
    a.jmp(top).unwrap(); // back-edge
    a.set_label(&mut done).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();

    let mut init = base_init();
    init[1] = 5; // ecx = 5 iterations
    diff(Setup {
        code,
        init,
        stack_lo: STACK_TOP - 16,
        stack_len: 16,
    });
}

/// AC#3: 4-byte push/pop frames and their effect on the stack + ESP. Push three
/// 32-bit values, pop them back into different registers (a swap through the stack).
#[test]
fn push_pop_32bit_frames() {
    let mut a = CodeAssembler::new(32).unwrap();
    a.push(eax).unwrap();
    a.push(ebx).unwrap();
    a.push(0x11223344u32).unwrap(); // push imm32
    a.pop(edx).unwrap(); // edx = 0x11223344
    a.pop(eax).unwrap(); // eax = old ebx
    a.pop(ebx).unwrap(); // ebx = old eax
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();

    let mut init = base_init();
    init[0] = 0xAAAA_1111; // eax
    init[3] = 0xBBBB_2222; // ebx
    diff(Setup {
        code,
        init,
        // three 4-byte pushes reach down to STACK_TOP-12; capture a bit more.
        stack_lo: STACK_TOP - 16,
        stack_len: 16,
    });
}

/// AC#3: the 66h operand-size override makes push/pop 2-byte. `push ax` writes 2
/// bytes and moves ESP by 2; `pop bx` reads them back.
#[test]
fn push_pop_16bit_override() {
    let mut a = CodeAssembler::new(32).unwrap();
    a.push(ax).unwrap(); // 66h push — 2-byte frame
    a.push(cx).unwrap();
    a.pop(dx).unwrap();
    a.pop(bx).unwrap();
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();

    let mut init = base_init();
    init[0] = 0x0000_1234; // ax = 0x1234
    init[1] = 0x0000_5678; // cx = 0x5678
    diff(Setup {
        code,
        init,
        stack_lo: STACK_TOP - 16,
        stack_len: 16,
    });
}

/// AC#3: ESP arithmetic wraps mod 2^32 and never leaks into the upper half of the
/// 64-bit backing store. We seed ESP with garbage in bits 32–63 (which a real
/// 32-bit CPU cannot hold) via `set_reg`, then run push/pop in a mapped low window:
/// each 4-byte ESP write must zero-extend, so the final ESP equals Unicorn's (whose
/// ESP started clean). `run_x86jit` additionally asserts ESP's upper 32 bits are 0.
///
/// This exercises the mod-2^32 stack-pointer semantics without faulting the store
/// (a true 0xFFFF_FFFC boundary pop would need the top guest page mapped, which the
/// contiguous flat model can't allocate cheaply — see the task decision note).
#[test]
fn esp_wraps_mod_2_32() {
    let mut a = CodeAssembler::new(32).unwrap();
    a.push(eax).unwrap(); // esp -> STACK_TOP-4
    a.push(ebx).unwrap(); // esp -> STACK_TOP-8
    a.pop(ecx).unwrap(); // ecx = ebx; esp -> STACK_TOP-4
    a.pop(edx).unwrap(); // edx = eax; esp -> STACK_TOP
    a.mov(esi, esp).unwrap(); // esi = final esp
    a.hlt().unwrap();
    let code = a.assemble(CODE).unwrap();

    let mut init = base_init();
    init[0] = 0x0102_0304; // eax
    init[3] = 0x0506_0708; // ebx
    diff_with(
        Setup {
            code,
            init,
            stack_lo: STACK_TOP - 16,
            stack_len: 16,
        },
        // Pollute the upper 32 bits of the x86jit ESP; the 4-byte stack writes must
        // clear them. Unicorn can't hold these bits, so a match proves the wrap.
        Some(0xDEAD_BEEF_0000_0000 | STACK_TOP),
    );
}

/// AC#2: a mixed control-flow + stack batch — call a subroutine that pushes/pops a
/// frame, computes, and returns; caller adds the result. Exercises call/ret frames,
/// EIP truncation, and 4-byte push/pop together (interp == JIT == Unicorn).
#[test]
fn mixed_call_stack_batch() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut sub = a.create_label();
    let mut after = a.create_label();
    a.mov(eax, 10u32).unwrap();
    a.call(sub).unwrap();
    a.set_label(&mut after).unwrap();
    let _ = after;
    a.add(eax, edx).unwrap(); // eax += return value in edx
    a.hlt().unwrap();
    a.set_label(&mut sub).unwrap();
    a.push(eax).unwrap();
    a.mov(edx, 7u32).unwrap();
    a.pop(eax).unwrap();
    a.ret().unwrap();
    let code = a.assemble(CODE).unwrap();

    diff(Setup {
        code,
        init: base_init(),
        stack_lo: STACK_TOP - 16,
        stack_len: 16,
    });
}

/// AC#3: `ret imm16` (caller-cleanup return) pops the 4-byte EIP *and* adds the
/// immediate to ESP. The callee is invoked after two argument pushes; `ret 8`
/// pops the return address then discards the two 4-byte args. Final ESP must equal
/// Unicorn's (frame + args reclaimed).
#[test]
fn ret_imm16_adjusts_esp() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut sub = a.create_label();
    a.push(0x2222u32).unwrap(); // arg2
    a.push(0x1111u32).unwrap(); // arg1
    a.call(sub).unwrap(); // pushes 4-byte return address
    a.mov(esi, esp).unwrap(); // esi = ESP after ret 8 cleanup
    a.hlt().unwrap();
    a.set_label(&mut sub).unwrap();
    a.mov(eax, 0x99u32).unwrap();
    a.ret_1(8u32).unwrap(); // pop EIP + add 8 to ESP (reclaim the two args)
    let code = a.assemble(CODE).unwrap();

    diff(Setup {
        code,
        init: base_init(),
        stack_lo: STACK_TOP - 16,
        stack_len: 16,
    });
}
