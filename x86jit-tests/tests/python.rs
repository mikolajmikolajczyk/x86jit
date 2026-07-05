//! Real-program forcing function, the interpreter summit (spec §12, testing.md
//! §12.5): drive an unmodified static-musl **CPython 3.13** and make `python3 -S
//! -c <script>` produce the same output three ways (native == interpreter ==
//! JIT). CPython is a large, real application — bytecode VM, GC, arbitrary-
//! precision ints, the import machinery — a stringent whole-pipeline exercise.
//!
//! The stdlib is served read-only from a checked-in minimal `pyhome` (just the
//! `encodings` package + a few modules `-S -c` touches; 3.13 freezes the rest);
//! `PYTHONHOME` points the interpreter at it, and `PYTHONDONTWRITEBYTECODE` keeps
//! the read-only tree from provoking `.pyc` writes.

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;
use x86jit_tests::reference::reference;

const FLAT: u64 = 0x800_0000; // 128 MiB
const HEAP_BASE: u64 = 0x200_0000; // past python's ~0x14a0000 bss end
const MMAP_BASE: u64 = 0x280_0000;
const STACK_TOP: u64 = 0x7f0_0000;

const PYHOME: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/programs/pyhome");

// A non-trivial program: recursion, list/dict comprehensions, Timsort, iterators,
// and arbitrary-precision integers (7**20 exceeds 64 bits). Real bytecode over the
// object model, not just a bare interpreter startup.
const SCRIPT: &str = "\
def fib(n): return n if n < 2 else fib(n - 1) + fib(n - 2)\n\
sq = [x * x for x in range(1, 11)]\n\
d = {c: ord(c) for c in 'abc'}\n\
s = sorted([3, 1, 4, 1, 5, 9, 2, 6])\n\
print(fib(20), sum(sq), d['b'], s, ''.join(reversed('python')), 7 ** 20)\n";

fn run_python(backend: Box<dyn Backend>) -> Vec<u8> {
    let home_env = format!("PYTHONHOME={PYHOME}");
    let argv: &[&[u8]] = &[b"python3", b"-S", b"-c", SCRIPT.as_bytes()];
    let envp: &[&[u8]] = &[home_env.as_bytes(), b"PYTHONDONTWRITEBYTECODE=1"];
    Guest::new_static(include_bytes!("../programs/python3.elf"))
        .flat(FLAT)
        .heap_base(HEAP_BASE)
        .mmap_base(MMAP_BASE)
        .stack_top(STACK_TOP)
        .argv(argv)
        .env(envp)
        .shim(|s| s.allow_dir(PYHOME))
        .run(backend)
}

#[test]
fn python_script_native_interp_jit_agree() {
    let reference = reference(
        b"6765 385 98 [1, 1, 2, 3, 4, 5, 6, 9] nohtyp 79792266297612001\n",
        || {
            std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/python3.elf"))
                .args(["-S", "-c", SCRIPT])
                .env("PYTHONHOME", PYHOME)
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .output()
                .expect("run native python")
                .stdout
        },
    );

    let interp = run_python(Box::new(InterpreterBackend));
    assert_eq!(interp, reference, "interpreter output != reference");
    let jit = run_python(Box::new(JitBackend::new()));
    assert_eq!(jit, reference, "JIT output != reference");
}
