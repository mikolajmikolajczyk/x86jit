//! Disassembly helper: decode guest bytes and format them (§12 M0).
//!
//! Inspection / debugging ONLY — no lift, no execution. This is the M0
//! "decoding loop that just prints" deliverable; the real lift to IR is §7 (M1).
//! Bitness is fixed at 64 (Long64); the `CpuMode` seam (§17.3) threads a variable
//! bitness in later — today the core is long-mode only.

use std::fmt::Write as _;

use iced_x86::{Decoder, DecoderOptions, Formatter, GasFormatter, Instruction};

const BITNESS: u32 = 64;

/// One decoded instruction: its guest address, raw encoding, and formatted text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedInsn {
    pub ip: u64,
    pub bytes: Vec<u8>,
    pub text: String,
}

impl DecodedInsn {
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Decode `code` as guest instructions starting at guest address `rip`.
///
/// Decodes the whole slice; invalid bytes surface as iced's `(bad)` text rather
/// than an error — this is a printer, not a validator. Uses AT&T syntax to line
/// up with `objdump -d` (the M0 acceptance oracle).
pub fn disassemble(code: &[u8], rip: u64) -> Vec<DecodedInsn> {
    let mut decoder = Decoder::with_ip(BITNESS, code, rip, DecoderOptions::NONE);
    let mut formatter = GasFormatter::new();
    // Match `objdump -d -M att` conventions so the two disassemblies line up
    // (the M0 acceptance oracle): lowercase hex, `$0x8` not `$8` for immediates,
    // and unpadded branch targets (`0x2b`, not `0x000…2B`).
    let opts = formatter.options_mut();
    opts.set_uppercase_hex(false);
    opts.set_small_hex_numbers_in_decimal(false);
    opts.set_branch_leading_zeros(false);
    let mut insn = Instruction::default();
    let mut text = String::new();
    let mut out = Vec::new();

    while decoder.can_decode() {
        decoder.decode_out(&mut insn);
        text.clear();
        formatter.format(&insn, &mut text);
        let start = (insn.ip() - rip) as usize;
        out.push(DecodedInsn {
            ip: insn.ip(),
            bytes: code[start..start + insn.len()].to_vec(),
            text: text.clone(),
        });
    }
    out
}

/// Format one decoded instruction as an `objdump`-style line:
/// `   401000:\t48 89 e5             \tmov    %rsp,%rbp`.
pub fn format_line(insn: &DecodedInsn) -> String {
    let mut bytes = String::new();
    for (i, b) in insn.bytes.iter().enumerate() {
        if i > 0 {
            bytes.push(' ');
        }
        write!(bytes, "{b:02x}").unwrap();
    }
    format!("{:>8x}:\t{:<21}\t{}", insn.ip, bytes, insn.text)
}

/// Decode `code` at `rip` and print each instruction, one per line.
pub fn print_disassembly(code: &[u8], rip: u64) {
    for insn in disassemble(code, rip) {
        println!("{}", format_line(&insn));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_byte_insns() {
        let nop = disassemble(&[0x90], 0x1000);
        assert_eq!(nop.len(), 1);
        assert_eq!(nop[0].ip, 0x1000);
        assert_eq!(nop[0].bytes, vec![0x90]);
        assert_eq!(nop[0].text, "nop");

        let ret = disassemble(&[0xc3], 0x1000);
        assert_eq!(ret[0].text, "ret");
    }

    #[test]
    fn multi_byte_insn_reports_length_and_bytes() {
        // 48 89 e5 = mov rbp, rsp  (AT&T: mov %rsp,%rbp)
        let d = disassemble(&[0x48, 0x89, 0xe5], 0x1000);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].len(), 3);
        assert_eq!(d[0].bytes, vec![0x48, 0x89, 0xe5]);
        assert!(d[0].text.starts_with("mov"), "got: {}", d[0].text);
    }

    #[test]
    fn decode_loop_advances_ip_per_instruction() {
        // nop; ret
        let d = disassemble(&[0x90, 0xc3], 0x400000);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].ip, 0x400000);
        assert_eq!(d[0].text, "nop");
        assert_eq!(d[1].ip, 0x400001);
        assert_eq!(d[1].text, "ret");
    }

    #[test]
    fn format_line_is_objdump_shaped() {
        let d = disassemble(&[0x48, 0x89, 0xe5], 0x401000);
        let line = format_line(&d[0]);
        assert!(line.starts_with("  401000:\t48 89 e5"), "got: {line}");
        assert!(line.ends_with(&d[0].text));
    }
}
