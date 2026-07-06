//! go-caddy P4 (netpoller): a static Go stdlib-`net` TCP server serves one real HTTP
//! response over a host-reachable socket, three ways (native / interp / JIT). This
//! exercises the whole netpoller substrate end to end — `epoll_create1`/`ctl`/`pwait`,
//! the `eventfd` netpollBreak wakeup, nonblocking `accept4`/`read`/`write` returning
//! `-EAGAIN` → epoll re-arm, and goroutine park/unpark across real host threads — over
//! the P1b Reserved span and the P2 threaded driver.
//!
//! Modeled on `socket.rs` (the pre-Go blocking server), extended to the threaded driver
//! and the Go/Reserved layout. No `net/http` surface: that's P5 (caddy endgame).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;

const SERVER: &[u8] = include_bytes!("../programs/tcpserve_go.elf");
const BODY: &[u8] = b"hello from go\n";

// The Go/Reserved layout the runner uses (matches go_hello.rs / x86jit-run P1b).
const GO_SPAN: u64 = 1 << 40;
const HEAP_BASE: u64 = 0x100_0000;
const BRK_LIMIT: u64 = 0x180_0000;
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
    s.write_all(b"GET / HTTP/1.0\r\n\r\n")
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
    let split = resp
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("HTTP header terminator");
    assert_eq!(&resp[split + 4..], BODY, "served body mismatch");
}

#[test]
fn go_server_serves_a_page_interp() {
    assert_http_ok(&serve_and_fetch(Box::new(InterpreterBackend)));
}

#[test]
fn go_server_serves_a_page_jit() {
    assert_http_ok(&serve_and_fetch(Box::new(JitBackend::new())));
}
