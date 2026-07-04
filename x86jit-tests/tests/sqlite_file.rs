//! Real-program forcing function, file-DB rung (INT-T10, testing.md §12.5): drive
//! the unmodified static-musl **sqlite3** against an on-disk database, with the SQL
//! script piped on **stdin** — `sqlite3 test.db < ops.sql`. Unlike the `:memory:`
//! variant this exercises the writable-file passthrough (`open` O_RDWR/O_CREAT,
//! `pwrite`, `ftruncate`, `fsync`, the `-journal`) plus a stdin buffer. Same output
//! three ways (native == interpreter == JIT).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use x86jit_core::{
    Backend, Exit, InterpreterBackend, MemConsistency, MemoryModel, Prot, Reg, RegionKind, Vm,
    VmConfig,
};
use x86jit_cranelift::JitBackend;
use x86jit_elf::{load_static_elf, setup_stack};
use x86jit_tests::reference::reference;
use x86jit_tests::syscall::LinuxShim;

const FLAT: u64 = 0x400_0000; // 64 MiB
const HEAP_BASE: u64 = 0x70_0000;
const MMAP_BASE: u64 = 0x100_0000;
const STACK_TOP: u64 = 0x3f0_0000;

const SQL: &[u8] = b"CREATE TABLE t(x INTEGER);\n\
    INSERT INTO t VALUES (1),(2),(3),(4),(5),(6),(7),(8),(9),(10);\n\
    SELECT sum(x*x), count(x), max(x*x) FROM t;\n";

/// A unique, empty database path under a per-process temp dir (with its journal
/// swept), so each run starts from a clean on-disk state.
fn fresh_db(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("x86jit-sqlite-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let seq = N.fetch_add(1, Ordering::Relaxed);
    let db = dir.join(format!("t-{tag}-{seq}.db"));
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_file(dir.join(format!("t-{tag}-{seq}.db-journal")));
    db
}

fn run_sqlite_file(backend: Box<dyn Backend>, db: &Path) -> Vec<u8> {
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
    let argv: &[&[u8]] = &[b"sqlite3", db.as_os_str().as_encoded_bytes()];
    let rsp = setup_stack(&mut vm, STACK_TOP, argv, &[b"PATH=/bin"]).unwrap();

    let mut cpu = vm.new_vcpu();
    cpu.set_reg(Reg::Rip, entry);
    cpu.set_reg(Reg::Rsp, rsp);

    let mut shim = LinuxShim::new();
    shim.brk = HEAP_BASE;
    shim.brk_limit = MMAP_BASE;
    shim.mmap_base = MMAP_BASE;
    shim.mmap_limit = STACK_TOP - 0x10_0000;
    shim.stdin = SQL.to_vec();
    shim.allow_write_dir(db.parent().unwrap());
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
fn sqlite_file_db_stdin_native_interp_jit_agree() {
    let reference = reference(b"385|10|100\n", || {
        let db = fresh_db("native");
        let mut child = Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/sqlite3.elf"))
            .arg(&db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn native sqlite3");
        child.stdin.take().unwrap().write_all(SQL).unwrap();
        child.wait_with_output().expect("native sqlite3").stdout
    });

    let interp = run_sqlite_file(Box::new(InterpreterBackend), &fresh_db("interp"));
    assert_eq!(interp, reference, "interpreter result != reference");
    let jit = run_sqlite_file(Box::new(JitBackend::new()), &fresh_db("jit"));
    assert_eq!(jit, reference, "JIT result != reference");
}
