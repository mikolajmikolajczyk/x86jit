//! Runtime loader for the third-party guest fixtures under `programs/`.
//!
//! Those binaries (busybox, sqlite3, the glibc/musl loaders, djpeg, lua, CPython,
//! the Go stand-ins) are other people's software and are **fetched + checksum-pinned**,
//! not committed — see `programs/MANIFEST.md`. Our own fixtures, built from the `.c`/
//! `.s`/`.go` sources in the same directory, stay committed and keep using
//! `include_bytes!`.
//!
//! The reason this module exists at all is that `include_bytes!` makes a fixture a
//! **compile-time** dependency: with the file absent, `cargo build --workspace` and
//! `cargo clippy --all-targets` fail to compile, and no test can skip its way past a
//! build error. Loading at run time is what lets a fixture be missing without taking
//! the workspace down with it.
//!
//! Fixtures are memoised and leaked to `&'static [u8]`, so a call site stays a
//! one-liner (`Guest::new_static(fixture::load("busybox.elf"))`) and the slice can be
//! moved into a thread. Test binaries are short-lived; the leak is the point, not an
//! oversight — it buys a `'static` lifetime for the price of a few MiB that the process
//! is about to drop anyway.
//!
//! # Policy
//!
//! `X86JIT_FIXTURES` selects what a missing fixture means:
//!
//! - **`require`** (the default, and what CI sets) — a missing fixture is a hard error
//!   naming the exact fetch command. Silence is how the 80286 corpus ended up never
//!   running in CI for months; the default must not be able to reduce coverage quietly.
//! - **`optional`** — an explicit local opt-in: a missing fixture makes its test skip
//!   with a note. A fixture that is *present but corrupt* is still a hard error under
//!   both policies.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// What a missing fixture means. From `X86JIT_FIXTURES`; anything unrecognised is
/// treated as `Require`, so a typo cannot silently weaken the gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    Require,
    Optional,
}

pub fn policy() -> Policy {
    match std::env::var("X86JIT_FIXTURES").as_deref() {
        Ok("optional") => Policy::Optional,
        _ => Policy::Require,
    }
}

/// The directory the fixtures live in. Resolved from *this* crate's manifest dir, so it
/// is correct no matter which crate calls (`x86jit-bench` has its own manifest dir).
pub fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("programs")
}

pub fn path(name: &str) -> std::path::PathBuf {
    dir().join(name)
}

fn cache() -> &'static Mutex<HashMap<String, &'static [u8]>> {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static [u8]>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Load a fixture, or return `None` when it is absent **and** the policy tolerates that.
///
/// Under `Require` an absent fixture panics rather than returning `None` — the caller
/// cannot accidentally turn a missing fixture into a pass. A read error that is *not*
/// "not found" always panics: a corrupt or unreadable fixture is never a skip.
pub fn try_load(name: &str) -> Option<&'static [u8]> {
    let mut cache = cache().lock().unwrap();
    if let Some(bytes) = cache.get(name) {
        return Some(*bytes);
    }
    let p = path(name);
    match std::fs::read(&p) {
        Ok(bytes) => {
            let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            cache.insert(name.to_string(), leaked);
            Some(leaked)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match policy() {
            Policy::Optional => None,
            Policy::Require => panic!(
                "guest fixture `{name}` is missing.\n  \
                 expected at: {}\n  \
                 fetch it:    x86jit-tests/programs/fetch-fixtures.sh {name}\n  \
                 (set X86JIT_FIXTURES=optional to skip the tests that need it instead)",
                p.display()
            ),
        },
        Err(e) => panic!(
            "guest fixture `{name}` at {} is unreadable: {e}",
            p.display()
        ),
    }
}

/// Load a fixture the caller cannot proceed without. Panics when absent under any policy.
pub fn load(name: &str) -> &'static [u8] {
    try_load(name).unwrap_or_else(|| {
        panic!(
            "guest fixture `{name}` is missing and this test cannot skip it — \
             run x86jit-tests/programs/fetch-fixtures.sh {name}"
        )
    })
}

/// Note that a test is skipping because a fixture is absent. Printed, not silent: a skip
/// nobody sees is indistinguishable from a pass.
pub fn skip(name: &str) {
    eprintln!(
        "SKIP: guest fixture `{name}` absent (X86JIT_FIXTURES=optional). \
         Run x86jit-tests/programs/fetch-fixtures.sh {name} to enable this test."
    );
}

/// Bind a fetched fixture at the top of a test, or return early with a note.
///
/// ```ignore
/// #[test]
/// fn busybox_wc() {
///     let image = fixture!("busybox.elf");
///     ...
/// }
/// ```
///
/// Under the default `require` policy the `None` arm is unreachable — `try_load` panics
/// first — so this reads as a skip but behaves as a hard failure unless someone opted
/// out deliberately.
#[macro_export]
macro_rules! fixture {
    ($name:literal) => {
        match $crate::fixture::try_load($name) {
            Some(bytes) => bytes,
            None => {
                $crate::fixture::skip($name);
                return;
            }
        }
    };
}

/// Guard a test whose fixtures are loaded further down (in a helper, say). Place it as
/// the first statement; under `optional` it notes the skip and returns, under the
/// default `require` the `try_load` inside panics first.
///
/// ```ignore
/// #[test]
/// fn busybox_wc() {
///     skip_without!("busybox.elf");
///     ...
/// }
/// ```
#[macro_export]
macro_rules! skip_without {
    ($($name:literal),+ $(,)?) => {
        $(
            if $crate::fixture::try_load($name).is_none() {
                $crate::fixture::skip($name);
                return;
            }
        )+
    };
}
