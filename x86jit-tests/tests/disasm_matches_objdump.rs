//! M0 acceptance (M0-T10, spec §12 M0): hand-assembled bytes decoded by the
//! core must match `objdump -d` on the same bytes.
//!
//! Differential test — `objdump` is the oracle (conventions §13). It runs the
//! real `objdump -M att` at test time and compares instruction text per address.
//! If `objdump` isn't on PATH the test SKIPS rather than fails, so it never
//! breaks a machine without binutils.

use std::io::Write;
use std::process::Command;

use x86jit_core::disassemble;
use x86jit_core::lift::CpuMode;

/// Hand-assembled long-mode corpus: reg/reg ALU, imm forms, stack ops, a memory
/// operand, a control-flow pair, and `syscall`. Curated to instructions whose
/// AT&T text is identical across iced and objdump.
const CODE: &[u8] = &[
    0x90, // nop
    0x48, 0x89, 0xe5, // mov %rsp,%rbp
    0x48, 0x01, 0xd8, // add %rbx,%rax
    0x48, 0x29, 0xc8, // sub %rcx,%rax
    0x48, 0x31, 0xc0, // xor %rax,%rax
    0x48, 0x83, 0xc0, 0x08, // add $0x8,%rax
    0x48, 0x39, 0xd8, // cmp %rbx,%rax
    0xb8, 0x01, 0x00, 0x00, 0x00, // mov $0x1,%eax
    0x50, // push %rax
    0x58, // pop %rax
    0x48, 0x8d, 0x04, 0x25, 0x00, 0x10, 0x00, 0x00, // lea 0x1000,%rax
    0x0f, 0x05, // syscall
    0xc3, // ret
    0xe8, 0x00, 0x00, 0x00, 0x00, // call 0x2b
    0x74, 0x05, // je 0x32
];

const BASE: u64 = 0;

/// Collapse whitespace and drop objdump's trailing `# ...` comments so the two
/// disassemblers' mnemonic padding doesn't cause spurious mismatches.
fn normalize(text: &str) -> String {
    let text = text.split('#').next().unwrap_or(text);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Run `objdump` over raw bytes; returns `(addr, normalized_text)` per decoded
/// instruction, or `None` if objdump can't be run.
fn objdump_disasm(bytes: &[u8]) -> Option<Vec<(u64, String)>> {
    let mut path = std::env::temp_dir();
    path.push(format!("x86jit_t10_{}.bin", std::process::id()));
    std::fs::File::create(&path).ok()?.write_all(bytes).ok()?;

    let output = Command::new("objdump")
        .args(["-D", "-b", "binary", "-m", "i386:x86-64", "-M", "att"])
        .arg(&path)
        .output();
    let _ = std::fs::remove_file(&path);

    let output = output.ok()?;
    if !output.status.success() {
        return None;
    }
    let dump = String::from_utf8_lossy(&output.stdout);

    let mut insns = Vec::new();
    for line in dump.lines() {
        // Instruction line: "   d:\t48 83 c0 08          \tadd    $0x8,%rax".
        // Byte-continuation lines ("  22:\t00 ") have no third field — skipped.
        let mut fields = line.splitn(3, '\t');
        let addr = fields.next().unwrap_or("").trim().trim_end_matches(':');
        let Ok(addr) = u64::from_str_radix(addr, 16) else {
            continue;
        };
        let _bytes = fields.next();
        let Some(text) = fields.next() else {
            continue;
        };
        insns.push((addr, normalize(text)));
    }
    Some(insns)
}

#[test]
fn disassembly_matches_objdump() {
    let Some(expected) = objdump_disasm(CODE) else {
        eprintln!("SKIP disassembly_matches_objdump: objdump unavailable");
        return;
    };

    let ours: Vec<(u64, String)> = disassemble(CODE, BASE, CpuMode::Long64)
        .iter()
        .map(|insn| (insn.ip, normalize(&insn.text)))
        .collect();

    assert_eq!(
        ours.len(),
        expected.len(),
        "instruction count differs\nours    = {ours:#?}\nobjdump = {expected:#?}"
    );
    for (ours, objdump) in ours.iter().zip(&expected) {
        assert_eq!(ours.0, objdump.0, "address mismatch");
        assert_eq!(
            ours.1, objdump.1,
            "text mismatch at {:#x}: ours={:?} objdump={:?}",
            ours.0, ours.1, objdump.1
        );
    }
}
