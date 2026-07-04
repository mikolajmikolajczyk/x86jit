//! Real-program forcing function, next rung (spec §12, testing.md §12.5): drive an
//! unmodified static-musl **sqlite3** and make an in-memory query produce the same
//! result three ways (native == interpreter == JIT). SQLite is a large, real
//! application — a free semantics fuzzer for the whole pipeline.
//!
//! `:memory:` avoids DB-file I/O; the SQL is a pure recursive computation, so the
//! output is deterministic regardless of the (optional) `/dev/urandom` seed the
//! CLI probes for.

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x400_0000; // 64 MiB
const HEAP_BASE: u64 = 0x70_0000; // past sqlite's ~0x61c000 bss end
const MMAP_BASE: u64 = 0x100_0000;
const STACK_TOP: u64 = 0x3f0_0000;

fn run_sqlite(backend: Box<dyn Backend>, argv: &[&[u8]]) -> Vec<u8> {
    let image = include_bytes!("../programs/sqlite3.elf");
    let mut vm = Vm::with_backend(
        VmConfig {
            memory_model: MemoryModel::Flat { size: FLAT },
            consistency: MemConsistency::Fast,
        },
        backend,
    );
    let entry = load_static_elf(&mut vm, image).expect("load sqlite3");
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

const SQL: &str = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<10) \
                   SELECT sum(x*x), count(x), max(x*x) FROM c;";

#[test]
fn sqlite_memory_query_native_interp_jit_agree() {
    let reference = reference(b"385|10|100\n", || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/sqlite3.elf"))
            .args([":memory:", SQL])
            .output()
            .expect("run native sqlite3")
            .stdout
    });

    let argv: &[&[u8]] = &[b"sqlite3", b":memory:", SQL.as_bytes()];
    let interp = run_sqlite(Box::new(InterpreterBackend), argv);
    let jit = run_sqlite(Box::new(JitBackend::new()), argv);
    assert_eq!(interp, reference, "interpreter result != reference");
    assert_eq!(jit, reference, "JIT result != reference");
}
