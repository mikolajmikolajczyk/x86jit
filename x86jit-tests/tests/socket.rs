//! A guest program serves a real, host-reachable TCP connection on x86jit
//! (go-caddy-plan.md Phase 0). `tcpserve.elf` is a freestanding raw-syscall server:
//! it `socket`/`bind`/`listen`/`accept`s on `127.0.0.1:<argv[1]>`, reads the request,
//! writes a fixed `HTTP/1.1 200` response, and exits. The shim forwards those socket
//! syscalls to real host fds, so this test — running entirely in-process — connects
//! from the host with `std::net::TcpStream` and gets the response back, byte-for-byte
//! identical under the interpreter and the JIT.
//!
//! This is the "web page served from the engine" milestone without Go: no threads,
//! no fork, one blocking connection. The socket plumbing it exercises (Fd::Socket,
//! sockaddr passthrough, accept) is the substrate Phase 4 extends with nonblocking
//! mode + epoll for the Go netpoller.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;

const SERVER: &[u8] = include_bytes!("../programs/tcpserve.elf");
/// The body `tcpserve.c` writes after the HTTP header (asserted verbatim).
const BODY: &[u8] = b"Served by x86jit\n";

/// A free loopback port: bind `:0`, read the assigned number, drop the listener.
/// A tiny race until the guest rebinds it, but the guest uses `SO_REUSEADDR` and the
/// listener never accepted a connection, so there's no `TIME_WAIT` to fight.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

/// Run `tcpserve.elf` under `backend` on its own thread (it blocks in `accept`),
/// connect from the host, send a request, and return the raw response bytes.
fn serve_and_fetch(backend: Box<dyn Backend>) -> Vec<u8> {
    let port = free_port();
    let port_s = port.to_string();

    let guest = thread::spawn(move || {
        let argv: [&[u8]; 2] = [b"tcpserve", port_s.as_bytes()];
        Guest::new_static(SERVER).argv(&argv).run(backend);
    });

    // The guest needs a moment to reach `listen`; retry the connect briefly.
    let mut stream = None;
    for _ in 0..200 {
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(s);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let mut s = stream.expect("guest server never accepted a connection");
    s.write_all(b"GET / HTTP/1.0\r\n\r\n")
        .expect("send request");
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let mut resp = Vec::new();
    s.read_to_end(&mut resp).expect("read response");
    guest.join().expect("guest thread panicked");
    resp
}

/// The response is a 200 whose body is the file `tcpserve` serves.
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
fn tcp_server_serves_a_page_interp() {
    assert_http_ok(&serve_and_fetch(Box::new(InterpreterBackend)));
}

#[test]
fn tcp_server_serves_a_page_jit() {
    assert_http_ok(&serve_and_fetch(Box::new(JitBackend::new())));
}
