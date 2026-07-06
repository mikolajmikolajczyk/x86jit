//! go-caddy P5 (caddy endgame): a static Go `net/http` file-server serves `index.html`
//! over a host-reachable socket through the full HTTP stack — `http.Server` request
//! parsing, the `http.FileServerFS` static handler, and graceful `Shutdown`, the same
//! surface caddy's `file_server` uses. This is the rung above `go_net.rs` (raw `net`),
//! over the P1b Reserved span and the P2 threaded driver.
//!
//! Serves three ways (native / interp / tiered JIT). The JIT leg runs with FD-TIER
//! tier-up (task-106) so Go's startup stays interpreted — see `serve_and_fetch` and the
//! note on `go_http_serves_index_jit` for why eager JIT alone fails (task-134).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;

const SERVER: &[u8] = include_bytes!("../programs/httpserve_go.elf");
const NEEDLE: &[u8] = b"hello from caddy-ish go";

// The Go/Reserved layout. The net/http binary is larger than tcpserve_go.elf: its
// BSS tops near 42 MiB, so the heap sits above that (go_net.rs can use a lower base).
const GO_SPAN: u64 = 1 << 40;
const HEAP_BASE: u64 = 0x400_0000;
const BRK_LIMIT: u64 = 0x480_0000;
const STACK_TOP: u64 = 0x8000_0000;
const MMAP_BASE: u64 = 0x1_0000_0000;
const MMAP_LIMIT: u64 = MMAP_BASE + (512 << 30);

/// A free loopback port: bind `:0`, read the assigned number, drop the listener. Go's
/// `net.Listen` sets `SO_REUSEADDR`, so the brief rebind race is harmless.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

/// Run the Go server under `backend` on the threaded driver (its own host thread — the
/// main goroutine parks in `epoll_pwait`), connect from the host, send a request, and
/// return the raw response bytes.
fn serve_and_fetch(backend: Box<dyn Backend>) -> Vec<u8> {
    let port = free_port();
    let port_s = port.to_string();

    let guest = thread::spawn(move || {
        let argv: [&[u8]; 2] = [b"tcpserve_go", port_s.as_bytes()];
        Guest::new_static(SERVER)
            .reserved(GO_SPAN)
            .heap_base(HEAP_BASE)
            .brk_limit(BRK_LIMIT)
            .mmap_base(MMAP_BASE)
            .mmap_limit(MMAP_LIMIT)
            .stack_top(STACK_TOP)
            .argv(&argv)
            // Tier up (FD-TIER, task-106): a block interprets until 50 executions, then
            // JIT-compiles. Go is startup-heavy and serves once here, so its time-sensitive
            // cold code (netpoller, deadline math) stays interpreted — dodging the
            // host-anchored clock racing under eager compilation (task-134) — while the hot
            // runtime loops (memclr/GC) still compile. No-op for the interpreter backend.
            .tier_up(Some(50))
            .run_threaded(backend);
    });

    // The Go runtime + netpoller take a moment to reach Accept; retry the connect. The
    // JIT compiles every block on first execution (no tier-up threshold here), so its
    // startup is far slower than the interpreter's — allow a generous window.
    let mut stream = None;
    for _ in 0..6000 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(s);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let mut s = stream.expect("go server never accepted a connection");
    // A real HTTP/1.1 request (Host required; Connection: close so the server drops the
    // socket after one response instead of holding a keep-alive open).
    s.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("send request");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

    let mut resp = Vec::new();
    s.read_to_end(&mut resp).expect("read response");
    guest.join().expect("guest thread panicked");
    resp
}

fn assert_http_ok(resp: &[u8]) {
    let text = String::from_utf8_lossy(resp);
    assert!(
        text.starts_with("HTTP/1.1 200 OK"),
        "expected a 200 response, got:\n{text}"
    );
    assert!(
        resp.windows(NEEDLE.len()).any(|w| w == NEEDLE),
        "served index.html body missing, got:\n{text}"
    );
}

#[test]
fn go_http_serves_index_interp() {
    assert_http_ok(&serve_and_fetch(Box::new(InterpreterBackend)));
}

// Runs under tier-up (see `serve_and_fetch`). Eager JIT alone fails here: it compiles
// every block on first execution (~100-400x slower than real time), so the host-anchored
// monotonic clock races from the guest's view (~19ms of wall-time between two adjacent
// time.Now() reads vs 45us interpreted) and net/http's deadlines blow before the
// connection goroutine writes its response — even though accept / epoll / request read /
// handler / response serialization are all verified correct (an httptest.Recorder
// produces byte-identical output interp==jit). Tier-up keeps that cold, time-sensitive
// code interpreted; task-134 (a virtual-monotonic clock) is the deeper fix for hot code
// that does compile.
#[test]
fn go_http_serves_index_jit() {
    assert_http_ok(&serve_and_fetch(Box::new(JitBackend::new())));
}
