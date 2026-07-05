//! Real-program forcing function, interpreter rung (spec §12, testing.md §12.5):
//! drive an unmodified static-musl **lua** and make a script's output match three
//! ways (native == interpreter == JIT). Lua numbers are IEEE doubles, so the VM's
//! arithmetic runs on SSE2, while musl's number parsing/formatting drags in the
//! x87 FPU (§14) — this is the first program to exercise x87 end to end.
//!
//! The script's output is a *string* verdict, robust to the last-bit differences
//! of our f64-backed (not 80-bit) x87: the comparisons that produce it are not
//! near ties, so the verdict is exact even though raw `%.14g` of a float might
//! differ in its final digits.

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;
use x86jit_tests::reference::reference;

const FLAT: u64 = 0x400_0000; // 64 MiB
const HEAP_BASE: u64 = 0x60_0000; // past lua's ~0x4b0000 bss end
const MMAP_BASE: u64 = 0x100_0000;
const STACK_TOP: u64 = 0x3f0_0000;

fn run_lua(backend: Box<dyn Backend>, argv: &[&[u8]]) -> Vec<u8> {
    Guest::new_static(include_bytes!("../programs/lua.elf"))
        .flat(FLAT)
        .heap_base(HEAP_BASE)
        .mmap_base(MMAP_BASE)
        .stack_top(STACK_TOP)
        .argv(argv)
        .env(&[b"PATH=/bin"])
        .run(backend)
}

// Tables, ipairs, double arithmetic (SSE2 + x87 in the number parse), a float
// compare, and string ops. Output is a *string* verdict (see the module docs).
const SCRIPT: &str = "local t={} for i=1,100 do t[i]=i*i end \
                      local s=0 for _,v in ipairs(t) do s=s+v end \
                      local ok = (s==338350) and (math.sqrt(2)>1.41 and math.sqrt(2)<1.42) \
                      print(ok and 'ok' or 'bad', string.rep('x',3))";

#[test]
fn lua_script_native_interp_jit_agree() {
    let reference = reference(b"ok\txxx\n", || {
        std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/programs/lua.elf"))
            .args(["-e", SCRIPT])
            .output()
            .expect("run native lua")
            .stdout
    });

    let argv: &[&[u8]] = &[b"lua", b"-e", SCRIPT.as_bytes()];
    let interp = run_lua(Box::new(InterpreterBackend), argv);
    let jit = run_lua(Box::new(JitBackend::new()), argv);
    assert_eq!(interp, reference, "interpreter output != reference");
    assert_eq!(jit, reference, "JIT output != reference");
}
