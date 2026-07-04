//! Real-program forcing function (spec §12, testing.md §12.5): drive busybox's
//! **gzip/gunzip** three ways (native == interpreter == JIT). DEFLATE is a fresh
//! instruction profile — heavy bit-shifting, Huffman table walks, LZ77 matching,
//! sliding-window copies. Decompression is fully deterministic; compression from
//! **stdin** is too (the gzip header's MTIME is 0 when the input isn't a file), so
//! both directions have stable baked expectations.

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x400_0000; // 64 MiB
const HEAP_BASE: u64 = 0x60_0000;
const MMAP_BASE: u64 = 0x100_0000;
const STACK_TOP: u64 = 0x3f0_0000;

const GZ_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/programs/gzip_input.txt.gz");

/// The exact bytes `gzip_input.txt` holds — what inflating the fixture yields, and
/// what compressing it back must consume.
const PLAIN: &[u8] = b"The quick brown fox jumps over the lazy dog.\n\
    Pack my box with five dozen liquor jugs.\n\
    How razorback-jumping frogs can level six piqued gymnasts!\n";

/// `busybox gzip -c` of `PLAIN` from stdin (MTIME=0 → deterministic).
const GZ_BYTES: &[u8] = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\x15\x8c\xcd\x01\x83\x20\x0c\x85\xef\x4e\xf1\x3a\x40\x9d\xa3\xc7\x1e\xba\x00\x68\x44\x5a\x21\x9a\x20\x2a\xd3\x9b\x9e\xbf\x9f\xcf\x4c\xd8\xf6\x38\xfc\xe0\x85\x8f\x8c\x89\x4f\x7c\xf7\xb4\x2a\xb8\x92\xa0\x18\x5e\x5c\xbb\x30\x72\xe8\xbb\xb7\x33\x2f\x5d\xf0\x26\x1d\xb1\xcc\x98\x62\x25\x43\x8d\x32\x96\xb8\xed\x2c\xd6\x06\xed\xbb\x17\x1f\x10\xd7\x58\xbc\x15\xcf\xff\x2f\xe6\x80\x49\x38\x28\x06\x67\x32\x55\x5a\xa0\xf1\xc4\x6a\x19\x8d\x08\x57\xca\x4e\x8b\x3e\xba\x1b\xa7\x68\x73\xc9\x91\x00\x00\x00";

/// Run busybox on `argv`, feeding `stdin` and permitting read of `allow`; return
/// captured stdout.
fn run(backend: Box<dyn Backend>, argv: &[&[u8]], stdin: &[u8], allow: Option<&str>) -> Vec<u8> {
    let image = include_bytes!("../programs/busybox.elf");
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    let entry = load_static_elf(&mut vm, image).expect("load busybox");
    vm.map(
        HEAP_BASE,
        (FLAT - HEAP_BASE) as usize,
        Prot::RW,
        RegionKind::Ram,
    )
    .unwrap();
    let rsp = setup_stack(&mut vm, STACK_TOP, argv, &[b"PATH=/bin"]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;
    shim.stdin = stdin.to_vec();
    if let Some(p) = allow {
        shim.allow_read(p);
    }
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

/// Inflate: `gunzip -c <fixture.gz>` → the original text.
#[test]
fn gunzip_inflate_native_interp_jit_agree() {
    let reference = reference(PLAIN, || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/busybox.elf"))
            .args(["gunzip", "-c", GZ_PATH])
            .output()
            .expect("run native busybox gunzip")
            .stdout
    });

    let argv: &[&[u8]] = &[b"busybox", b"gunzip", b"-c", GZ_PATH.as_bytes()];
    let interp = run(Box::new(InterpreterBackend), argv, &[], Some(GZ_PATH));
    assert_eq!(interp, reference, "interpreter inflate != reference");
    let jit = run(Box::new(JitBackend::new()), argv, &[], Some(GZ_PATH));
    assert_eq!(jit, reference, "JIT inflate != reference");
}

/// Deflate: `gzip -c` of the text on stdin → the compressed bytes (LZ77 + Huffman).
#[test]
fn gzip_deflate_native_interp_jit_agree() {
    let reference = reference(GZ_BYTES, || {
        use std::io::Write;
        let mut child = std::process::Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/programs/busybox.elf"
        ))
        .args(["gzip", "-c"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn native busybox gzip");
        child.stdin.take().unwrap().write_all(PLAIN).unwrap();
        child
            .wait_with_output()
            .expect("native busybox gzip")
            .stdout
    });

    let argv: &[&[u8]] = &[b"busybox", b"gzip", b"-c"];
    let interp = run(Box::new(InterpreterBackend), argv, PLAIN, None);
    assert_eq!(interp, reference, "interpreter deflate != reference");
    let jit = run(Box::new(JitBackend::new()), argv, PLAIN, None);
    assert_eq!(jit, reference, "JIT deflate != reference");
}
