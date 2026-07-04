//! Real-program forcing function, the SIMD-codec rung (spec §12, testing.md
//! §12.5): drive **libjpeg-turbo's `djpeg`** — an unmodified, production JPEG
//! decoder — to decode a real `.jpg` to a PPM three ways (native == interpreter
//! == JIT). This is the heaviest SIMD workload on the engine yet: the inverse DCT,
//! dequantization, upsampling, and YCbCr→RGB all run through libjpeg-turbo's
//! hand-written SSE2/SSSE3 kernels (selected via `cpuid` — the x86-64-v2 baseline
//! the engine advertises picks the SSE path, not AVX2). Integer IDCT is
//! bit-exact, so the decoded pixels are a stable baked expectation.

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x400_0000; // 64 MiB
const HEAP_BASE: u64 = 0x80_0000;
const MMAP_BASE: u64 = 0x100_0000;
const STACK_TOP: u64 = 0x3f0_0000;

const JPG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/programs/djpeg_input.jpg");
/// The decoded PPM (binary P6) libjpeg-turbo produces for the fixture.
const EXPECTED: &[u8] = include_bytes!("../programs/djpeg_expected.ppm");

fn run_djpeg(backend: Box<dyn Backend>) -> Vec<u8> {
    let image = include_bytes!("../programs/djpeg.elf");
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    let entry = load_static_elf(&mut vm, image).expect("load djpeg");
    vm.map(
        HEAP_BASE,
        (FLAT - HEAP_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();
    let argv: &[&[u8]] = &[b"djpeg", b"-pnm", JPG.as_bytes()];
    let rsp = setup_stack(&mut vm, STACK_TOP, argv, &[b"PATH=/bin"]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;
    shim.allow_read(JPG);
    loop {
        match cpu.run(&vm, None) {
            Exit::Syscall => {
                if shim.handle(&mut cpu, &mut vm) {
                    break;
                }
            }
            other => panic!("gap at rip={:#x}: {other:?}", cpu.reg(Reg::Rip)),
        }
    }
    shim.stdout
}

#[test]
fn djpeg_decode_native_interp_jit_agree() {
    let reference = reference(EXPECTED, || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/djpeg.elf"))
            .args(["-pnm", JPG])
            .output()
            .expect("run native djpeg")
            .stdout
    });

    let interp = run_djpeg(Box::new(InterpreterBackend));
    assert_eq!(interp, reference, "interpreter decode != reference");
    let jit = run_djpeg(Box::new(JitBackend::new()));
    assert_eq!(jit, reference, "JIT decode != reference");
}
