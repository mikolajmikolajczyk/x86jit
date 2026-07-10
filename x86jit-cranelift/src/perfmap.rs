//! Env-gated Linux `perf` symbol map for JIT-compiled guest blocks (task-196).
//!
//! When `X86JIT_PERF_MAP=1` is set in the environment, each compiled block/region
//! is recorded to `/tmp/perf-<pid>.map` using the standard perf JIT convention:
//! one line per symbol, `<hex start> <hex size> <name>\n`, no `0x` prefixes. Linux
//! `perf report`/`perf annotate` reads this file to attribute samples that land in
//! JIT'd host code to a named symbol — here `jit_0x<guest_rip>` — so an embedder
//! (unemups4) can see which guest blocks are hot.
//!
//! **Zero cost when off.** [`record`] does a single `OnceLock` get and an
//! `is_none` branch on the (cold) compile path; nothing is emitted into guest
//! machine code, and no file is touched. The env var is read exactly once.
//!
//! **Serialization.** The writer is a `Mutex<LineWriter<File>>`, so foreground and
//! background (tier-up) compile threads append without interleaving lines.
//!
//! **Accepted limitations (see task-196):**
//! - Entries are append-only and never retracted. cranelift-jit never frees
//!   compiled code, so a stale symbol never points at reused host memory; a block
//!   dropped by SMC keeps its bytes, so its range stays valid. This matches
//!   `codemap`'s own append-only lifetime model.
//! - DWARF unwind information does not cross JIT frames, so `perf` cannot unwind
//!   *through* a JIT frame into its caller. Samples inside a block attribute flat
//!   to that block's symbol — sufficient for "which guest blocks are hot", which
//!   is the goal.
//!
//! This module lives in `x86jit-cranelift` (not `x86jit-core`): it does file I/O
//! and reads the environment, which would violate core's pure-data constraint
//! (see `x86jit-core/src/codemap.rs`).

use std::fmt::Write as _;
use std::fs::File;
use std::io::{LineWriter, Write};
use std::sync::{Mutex, OnceLock};

/// The compilation unit a recorded symbol names, controlling its symbol prefix.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    /// A single compiled block — symbol `jit_0x<guest_start>`.
    Block,
    /// A compiled superblock region — symbol `jit_region_0x<entry>`.
    Region,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Block => "jit_0x",
            Kind::Region => "jit_region_0x",
        }
    }
}

/// Process-global perf-map writer. `Some` iff `X86JIT_PERF_MAP=1` at first use;
/// the env var is read exactly once and the file opened lazily.
static WRITER: OnceLock<Option<Mutex<LineWriter<File>>>> = OnceLock::new();

/// Open `/tmp/perf-<pid>.map` for append iff `X86JIT_PERF_MAP=1`. Any failure to
/// read the env or open the file degrades to `None` (perf map simply disabled) —
/// this is a diagnostic aid and must never affect execution.
fn init() -> Option<Mutex<LineWriter<File>>> {
    if std::env::var_os("X86JIT_PERF_MAP").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return None;
    }
    let path = format!("/tmp/perf-{}.map", std::process::id());
    File::create(path)
        .ok()
        .map(|f| Mutex::new(LineWriter::new(f)))
}

/// Format one perf-map line into `out`: `<hex start> <hex size> <prefix><hex guest>\n`,
/// no `0x` prefixes on the address/size fields (perf's expected format). Split out
/// from the file I/O so it is unit-testable against any `impl Write` (see tests).
fn format_line(
    out: &mut impl Write,
    start: usize,
    len: u32,
    kind: Kind,
    guest: u64,
) -> std::io::Result<()> {
    // Build the symbol name without a second allocation-per-call heap string where
    // possible; `write!` into a small reused stack buffer via a String is fine on
    // this cold path.
    let mut name = String::with_capacity(kind.prefix().len() + 16);
    let _ = write!(name, "{}{guest:x}", kind.prefix());
    writeln!(out, "{start:x} {len:x} {name}")
}

/// Record a compiled unit's host range under the guest RIP it was compiled from.
/// No-op (one `OnceLock` get + `is_none` branch) unless `X86JIT_PERF_MAP=1`.
///
/// `start`/`len` are the host code range (as registered in `codemap`); `guest` is
/// the guest entry PC (`ir.guest_start` for a block, `region.entry` for a region).
pub(crate) fn record(start: usize, len: u32, kind: Kind, guest: u64) {
    let Some(writer) = WRITER.get_or_init(init) else {
        return;
    };
    // A poisoned lock (a prior panic while holding it) must not take down the JIT;
    // recover the guard and keep emitting.
    let mut w = writer.lock().unwrap_or_else(|e| e.into_inner());
    // Ignore write errors: perf-map emission is best-effort diagnostics.
    let _ = format_line(&mut *w, start, len, kind, guest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_line_block_is_well_formed_no_0x_prefixes() {
        let mut buf = Vec::new();
        format_line(&mut buf, 0x7f00_1000, 0x40, Kind::Block, 0x0040_3146).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "7f001000 40 jit_0x403146\n"
        );
    }

    #[test]
    fn format_line_region_uses_region_prefix() {
        let mut buf = Vec::new();
        format_line(&mut buf, 0x1000, 0x1_2345, Kind::Region, 0x400000).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "1000 12345 jit_region_0x400000\n"
        );
    }

    #[test]
    fn format_line_handles_zero_and_large_values() {
        let mut buf = Vec::new();
        format_line(&mut buf, 0, 0, Kind::Block, 0).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "0 0 jit_0x0\n");

        let mut buf = Vec::new();
        format_line(
            &mut buf,
            usize::MAX,
            u32::MAX,
            Kind::Block,
            0xdead_beef_cafe_babe,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            format!("{:x} ffffffff jit_0xdeadbeefcafebabe\n", usize::MAX)
        );
    }
}
