//! Real16 (16-bit real-mode) segmented-addressing + control-flow differential.
//!
//! Companion to `cf32.rs`, but for `CpuMode::Real16`. Each case hand-assembles a
//! 16-bit snippet and runs it two ways — the x86jit **interpreter** (the JIT does
//! not support Real16, §17.6) and Unicorn in `MODE_16` — then asserts they agree
//! on the final GPRs (AX/CX/DX/BX/SP/BP/SI/DI, low 16 bits), IP, and the specific
//! memory bytes each snippet touches.
//!
//! The CS:IP fetch model is exercised directly: `cpu.rip` holds the 16-bit IP
//! offset within CS, and the physical fetch address is `(CS << 4) + IP`. Data
//! accesses are `segment_base + (offset & 0xFFFF)` with iced's effective-segment
//! rule (DS default, SS for BP-based operands, `es:`/`ss:` overrides honored), and
//! the stack lives at SS:SP with SP wrapping mod 2^16.

#![cfg(feature = "unicorn")]

use x86jit_core::lift::CpuMode;
use x86jit_core::{Exit, InterpreterBackend, Prot, Reg, RegionKind, Vm, VmConfig};

use unicorn_engine::unicorn_const::{Arch, Mode, Prot as UProt};
use unicorn_engine::{RegisterX86, Unicorn};

/// Flat guest space large enough to hold every physical address any snippet uses.
/// The largest base is SS = 0x300 (base 0x3000) plus a wrapped SP of 0xFFFE, i.e.
/// ~0x3FFFE, so 0x40000 covers it with room to spare (and is page-aligned).
const FLAT: u64 = 0x4_0000;

/// The 16-bit GPRs we compare (encoding order — SP included, that's the point).
const GPRS: [(Reg, RegisterX86); 8] = [
    (Reg::Rax, RegisterX86::AX),
    (Reg::Rcx, RegisterX86::CX),
    (Reg::Rdx, RegisterX86::DX),
    (Reg::Rbx, RegisterX86::BX),
    (Reg::Rsp, RegisterX86::SP),
    (Reg::Rbp, RegisterX86::BP),
    (Reg::Rsi, RegisterX86::SI),
    (Reg::Rdi, RegisterX86::DI),
];

/// A byte region to seed before the run and read back afterwards, keyed by its
/// **physical** guest address (both engines share the same flat layout).
#[derive(Clone)]
struct MemRegion {
    phys: u64,
    /// Bytes to write before running (the "seed"); its length is also the number
    /// of bytes read back and compared afterwards.
    init: Vec<u8>,
}

struct Setup {
    /// Human label so a failing assert names the snippet.
    name: &'static str,
    /// Hand-assembled 16-bit machine code, placed at physical `(cs << 4) + ip`.
    code: Vec<u8>,
    cs: u64,
    ds: u64,
    es: u64,
    ss: u64,
    ip: u16,
    /// Initial 16-bit GPR values (encoding order); index 4 (SP) is honored.
    init: [u16; 8],
    /// Data/stack byte regions to seed and to compare after the run.
    mem: Vec<MemRegion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    gpr: [u16; 8],
    ip: u16,
    /// Read-back of each `Setup::mem` region, in order.
    mem: Vec<Vec<u8>>,
}

/// Run a snippet on an x86jit `Vm` (interpreter backend) in Real16.
fn run_x86jit(setup: &Setup) -> Outcome {
    let mut vm = Vm::with_backend(VmConfig::flat(FLAT), Box::new(InterpreterBackend));
    vm.set_cpu_mode(CpuMode::Real16);
    vm.map(0, FLAT as usize, Prot::RWX, RegionKind::Ram)
        .unwrap();

    let code_phys = (setup.cs << 4) + setup.ip as u64;
    vm.write_bytes(code_phys, &setup.code).unwrap();
    for r in &setup.mem {
        vm.write_bytes(r.phys, &r.init).unwrap();
    }

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Cs, setup.cs);
    cpu.set_reg(Reg::Ds, setup.ds);
    cpu.set_reg(Reg::Es, setup.es);
    cpu.set_reg(Reg::Ss, setup.ss);
    cpu.set_reg(Reg::Rip, setup.ip as u64);
    for (i, (reg, _)) in GPRS.iter().enumerate() {
        cpu.set_reg(*reg, setup.init[i] as u64);
    }

    match cpu.run(&vm, Some(10_000)) {
        Exit::Hlt => {}
        other => panic!("[{}] x86jit did not hlt: {other:?}", setup.name),
    }

    let mut gpr = [0u16; 8];
    for (i, (reg, _)) in GPRS.iter().enumerate() {
        gpr[i] = cpu.reg(*reg) as u16;
    }
    let mut mem = Vec::with_capacity(setup.mem.len());
    for r in &setup.mem {
        let mut buf = vec![0u8; r.init.len()];
        vm.read_bytes(r.phys, &mut buf).unwrap();
        mem.push(buf);
    }
    Outcome {
        gpr,
        ip: cpu.reg(Reg::Rip) as u16,
        mem,
    }
}

/// Run the same snippet on Unicorn in `MODE_16`. Segment selectors are set with
/// `reg_write`; Unicorn recomputes the base as `selector << 4` internally.
fn run_unicorn(setup: &Setup) -> Outcome {
    let mut uc = Unicorn::new(Arch::X86, Mode::MODE_16).expect("open unicorn x86-16");
    uc.mem_map(0, FLAT, UProt::ALL).expect("map");

    let code_phys = (setup.cs << 4) + setup.ip as u64;
    uc.mem_write(code_phys, &setup.code).expect("write code");
    for r in &setup.mem {
        uc.mem_write(r.phys, &r.init).expect("seed mem");
    }

    uc.reg_write(RegisterX86::CS, setup.cs).unwrap();
    uc.reg_write(RegisterX86::DS, setup.ds).unwrap();
    uc.reg_write(RegisterX86::ES, setup.es).unwrap();
    uc.reg_write(RegisterX86::SS, setup.ss).unwrap();
    uc.reg_write(RegisterX86::IP, setup.ip as u64).unwrap();
    for (i, (_, ureg)) in GPRS.iter().enumerate() {
        uc.reg_write(*ureg, setup.init[i] as u64).unwrap();
    }

    // Stop just before the terminating hlt (privileged in Unicorn) and record its
    // *linear* address so we can convert back to the IP offset within CS.
    use std::cell::Cell;
    use std::rc::Rc;
    let hlt_at: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
    let h = hlt_at.clone();
    uc.add_code_hook(0, u64::MAX, move |uc, addr, size| {
        let mut buf = vec![0u8; size as usize];
        if uc.mem_read(addr, &mut buf).is_ok() && size == 1 && buf[0] == 0xf4 {
            h.set(Some(addr));
            let _ = uc.emu_stop();
        }
    })
    .expect("hook");

    let _ = uc.emu_start(code_phys, u64::MAX, 0, 10_000);

    let mut gpr = [0u16; 8];
    for (i, (_, ureg)) in GPRS.iter().enumerate() {
        gpr[i] = uc.reg_read(*ureg).unwrap() as u16;
    }
    // Engine convention (mirrors cf32): IP resumes past the terminating hlt.
    // Convert the linear hlt address back to an IP offset within CS, then +1.
    let cs_base = setup.cs << 4;
    let ip = match hlt_at.get() {
        Some(a) => ((a - cs_base + 1) & 0xFFFF) as u16,
        None => uc.reg_read(RegisterX86::IP).unwrap() as u16,
    };
    let mut mem = Vec::with_capacity(setup.mem.len());
    for r in &setup.mem {
        mem.push(uc.mem_read_as_vec(r.phys, r.init.len()).unwrap());
    }
    Outcome { gpr, ip, mem }
}

/// The differential: x86jit-interpreter (Real16) must equal Unicorn (MODE_16).
fn diff(setup: Setup) {
    let interp = run_x86jit(&setup);
    let unicorn = run_unicorn(&setup);
    assert_eq!(
        interp, unicorn,
        "[{}] x86jit-interp vs Unicorn-16 diverge\n  code={}\n  cs={:#x} ds={:#x} es={:#x} ss={:#x} ip={:#x}\n  init={:x?}\n  interp={interp:x?}\n  unicorn={unicorn:x?}",
        setup.name,
        setup.code.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        setup.cs, setup.ds, setup.es, setup.ss, setup.ip, setup.init,
    );
}

fn zero_init() -> [u16; 8] {
    [0u16; 8]
}

/// Little-endian 16-bit word helper for seeding/expecting data.
fn le16(v: u16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// #1 — DS-segmented load + store. CS<<4 = 0x1000 fetch base; DS<<4 = 0x2000 data
/// base. `mov bx,0x10` / `mov ax,[bx]` (loads DS:0x10) / `mov [bx+2],ax` (stores to
/// DS:0x12) / hlt. Proves the DS base is applied to both the load and the store.
///
/// Encoding:  BB 10 00   mov bx,0x10
///            8B 07      mov ax,[bx]
///            89 47 02   mov [bx+2],ax
///            F4         hlt
#[test]
fn ds_segmented_load_store() {
    let code = vec![0xBB, 0x10, 0x00, 0x8B, 0x07, 0x89, 0x47, 0x02, 0xF4];
    diff(Setup {
        name: "ds_segmented_load_store",
        code,
        cs: 0x100, // base 0x1000
        ds: 0x200, // base 0x2000
        es: 0x000,
        ss: 0x300,
        ip: 0,
        init: zero_init(),
        mem: vec![
            // DS:0x10 (phys 0x2010) seeded with the value AX will load.
            MemRegion {
                phys: 0x2010,
                init: le16(0xBEEF),
            },
            // DS:0x12 (phys 0x2012) — target of the store; seed to a wrong value.
            MemRegion {
                phys: 0x2012,
                init: le16(0x0000),
            },
        ],
    });
}

/// #2 — SS is used for BP-based operands (not DS). SS<<4 = 0x3000, DS<<4 = 0x2000.
/// `mov bp,0x20` / `mov ax,[bp]` / hlt. `[bp]` addresses SS:0x20 (phys 0x3020). A
/// decoy word sits at DS:0x20 (phys 0x2020) to prove SS — not DS — is chosen.
///
/// Encoding:  BD 20 00   mov bp,0x20
///            8B 46 00   mov ax,[bp+0]
///            F4         hlt
#[test]
fn ss_via_bp() {
    let code = vec![0xBD, 0x20, 0x00, 0x8B, 0x46, 0x00, 0xF4];
    diff(Setup {
        name: "ss_via_bp",
        code,
        cs: 0x100, // base 0x1000
        ds: 0x200, // base 0x2000
        es: 0x000,
        ss: 0x300, // base 0x3000
        ip: 0,
        init: zero_init(),
        mem: vec![
            // SS:0x20 (phys 0x3020) — the real value BP-relative load must return.
            MemRegion {
                phys: 0x3020,
                init: le16(0x5EED),
            },
            // DS:0x20 (phys 0x2020) — decoy; if the engine wrongly used DS, AX = 0xDEAD.
            MemRegion {
                phys: 0x2020,
                init: le16(0xDEAD),
            },
        ],
    });
}

/// #3 — ES segment override (prefix 0x26). ES<<4 = 0x4000, DS<<4 = 0x2000.
/// `mov bx,0x30` / `mov ax,es:[bx]` / hlt. Reads ES:0x30 (phys 0x4030); a decoy at
/// DS:0x30 (phys 0x2030) proves the override picks the ES base.
///
/// Encoding:  BB 30 00      mov bx,0x30
///            26 8B 07      mov ax,es:[bx]
///            F4            hlt
#[test]
fn es_segment_override() {
    let code = vec![0xBB, 0x30, 0x00, 0x26, 0x8B, 0x07, 0xF4];
    diff(Setup {
        name: "es_segment_override",
        code,
        cs: 0x100, // base 0x1000
        ds: 0x200, // base 0x2000
        es: 0x400, // base 0x4000
        ss: 0x300,
        ip: 0,
        init: zero_init(),
        mem: vec![
            // ES:0x30 (phys 0x4030) — the real value the override must load.
            MemRegion {
                phys: 0x4030,
                init: le16(0xF00D),
            },
            // DS:0x30 (phys 0x2030) — decoy for the default segment.
            MemRegion {
                phys: 0x2030,
                init: le16(0xDEAD),
            },
        ],
    });
}

/// #4 — 16-bit effective-offset wrap across the top of a segment. DS<<4 = 0x2000.
/// `mov bx,0xFFFF` / `mov ax,[bx+3]` / hlt. The effective offset is
/// (0xFFFF + 3) & 0xFFFF = 0x0002, so the access lands at DS:0x0002 (phys 0x2002),
/// NOT at 0x2000 + 0x10002. Both engines must agree the offset wrapped.
///
/// Encoding:  BB FF FF   mov bx,0xFFFF
///            8B 47 03   mov ax,[bx+3]
///            F4         hlt
#[test]
fn offset_wraps_within_segment() {
    let code = vec![0xBB, 0xFF, 0xFF, 0x8B, 0x47, 0x03, 0xF4];
    diff(Setup {
        name: "offset_wraps_within_segment",
        code,
        cs: 0x100, // base 0x1000
        ds: 0x200, // base 0x2000
        es: 0x000,
        ss: 0x300,
        ip: 0,
        init: zero_init(),
        mem: vec![
            // Where the wrapped offset lands: DS:0x0002 (phys 0x2002).
            MemRegion {
                phys: 0x2002,
                init: le16(0xCAFE),
            },
            // The non-wrapped (buggy) location DS:0x10002 would be phys 0x12002;
            // seed a distinct value so a non-wrapping engine would return it instead.
            MemRegion {
                phys: 0x1_2002,
                init: le16(0xDEAD),
            },
        ],
    });
}

/// #5 — near `call rel16` + `ret`. CS<<4 = 0x1000, SS<<4 = 0x3000, SP = 0x100.
/// `call sub` pushes the 2-byte return IP (0x0003) at SS:SP, jumps to the callee,
/// which sets BX and `ret`s; execution resumes at `inc ax`. Pins the 2-byte call
/// frame, the SS:SP push, and IP/SP restoration.
///
/// Encoding (IP offsets shown):
///   0000: E8 02 00   call 0x0005      (rel16 = 0x0002, target = 0x0003 + 0x0002)
///   0003: 40         inc ax           (executes after ret)
///   0004: F4         hlt
///   0005: BB 34 12   mov bx,0x1234    (callee)
///   0008: C3         ret
#[test]
fn near_call_ret() {
    let code = vec![
        0xE8, 0x02, 0x00, // call 0x0005
        0x40, // inc ax
        0xF4, // hlt
        0xBB, 0x34, 0x12, // mov bx,0x1234
        0xC3, // ret
    ];
    let mut init = zero_init();
    init[4] = 0x0100; // SP
    diff(Setup {
        name: "near_call_ret",
        code,
        cs: 0x100, // base 0x1000
        ds: 0x200,
        es: 0x000,
        ss: 0x300, // base 0x3000
        ip: 0,
        init,
        // The pushed return address lands at SS:(SP-2) = SS:0xFE (phys 0x30FE).
        mem: vec![MemRegion {
            phys: 0x30FE,
            init: le16(0x0000),
        }],
    });
}

/// #6 — push/pop with SP wrap at zero. SS<<4 = 0x3000, SP = 0x0000. The first
/// `push ax` predecrements SP mod 2^16 to 0xFFFE and writes AX at SS:0xFFFE
/// (phys 0x3FFFE); `pop bx` reads it back and SP returns to 0. Exercises the
/// mod-2^16 stack-pointer wrap and the SS:SP stack base together.
///
/// Encoding:  B8 CD AB   mov ax,0xABCD
///            50         push ax
///            5B         pop bx
///            F4         hlt
#[test]
fn push_pop_sp_wrap() {
    let code = vec![0xB8, 0xCD, 0xAB, 0x50, 0x5B, 0xF4];
    let mut init = zero_init();
    init[4] = 0x0000; // SP = 0 -> push wraps it to 0xFFFE
    diff(Setup {
        name: "push_pop_sp_wrap",
        code,
        cs: 0x100, // base 0x1000
        ds: 0x200,
        es: 0x000,
        ss: 0x300, // base 0x3000
        ip: 0,
        init,
        // The pushed word lands at SS:0xFFFE (phys 0x3FFFE).
        mem: vec![MemRegion {
            phys: 0x3_FFFE,
            init: le16(0x0000),
        }],
    });
}
