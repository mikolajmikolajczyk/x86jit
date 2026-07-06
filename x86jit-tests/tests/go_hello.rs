//! go-caddy Go acceptance. P1b: the Go build-note heuristic that selects the Reserved
//! span + threaded driver. P3 (below, once the signal stubs land) will add the
//! three-way "static Go hello prints hello" run through the production shim.

use x86jit_elf::has_go_build_note;

/// P1b: a Go entrypoint is recognized by its `PT_NOTE` build note (which survives
/// `strip`/`-s -w`), and a non-Go static ELF is not — this is what the runner keys the
/// Reserved-span + threaded-driver choice off.
#[test]
fn go_build_note_detected_only_for_go() {
    let go = include_bytes!("../programs/hello_go.elf");
    let not_go = include_bytes!("../programs/hello_static.elf");
    assert!(has_go_build_note(go), "Go binary carries the Go build note");
    assert!(
        !has_go_build_note(not_go),
        "a musl static ELF has no Go build note"
    );
    assert!(!has_go_build_note(b"not an elf"), "garbage isn't Go");
}
