//! go-caddy P5-real (task-153): the **actual** caddy binary (`caddy file-server`)
//! serves `index.html` over a host-reachable socket, curl-able from the host — the
//! rung above the `httpserve_go.elf` net/http stand-in (`go_http.rs`). Exercises
//! caddy's real HTTP stack (request parsing, the `file_server` handler, the Go
//! netpoller) over the P1b Reserved span and the P2 threaded driver.
//!
//! Runs three ways: native (host control) / interp / tiered JIT.
//!
//! `caddy.elf` (~52 MiB static Go) is a large, moving-target fixture, so it is
//! git-ignored and built locally (see `x86jit-tests/programs/README` / task-153).
//! When it is absent this test no-ops with a note instead of failing — mirroring
//! the git-ignored OCI fixtures (`x86jit-oci/fixtures/README.md`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use x86jit_core::{Backend, InterpreterBackend};
use x86jit_cranelift::JitBackend;
use x86jit_tests::guest::Guest;

const NEEDLE: &[u8] = b"x86jit-caddy-lives";

// caddy's RW/BSS tops near 88 MiB, so the heap sits at 0x600_0000 (task-161).
const GO_SPAN: u64 = 1 << 40;
const HEAP_BASE: u64 = 0x600_0000;
const BRK_LIMIT: u64 = 0x680_0000;
const STACK_TOP: u64 = 0x8000_0000;
const MMAP_BASE: u64 = 0x1_0000_0000;
const MMAP_LIMIT: u64 = MMAP_BASE + (512 << 30);

/// The git-ignored fixture path; absent on a fresh checkout / CI without a local build.
fn caddy_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("programs/caddy.elf")
}

fn skip_note() {
    eprintln!(
        "skipping: x86jit-tests/programs/caddy.elf not present \
         (build locally: CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -o \
         x86jit-tests/programs/caddy.elf the caddy cmd; see task-153)"
    );
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A temp rootfs with `index.html` (the NEEDLE) plus a writable HOME/XDG dir. The
/// returned guard removes the tree on drop.
struct Fixture {
    base: PathBuf,
    srv: PathBuf,
    home: PathBuf,
}
impl Fixture {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("x86jit-caddy-{}-{}", tag, std::process::id()));
        let srv = base.join("srv");
        let home = base.join("home");
        std::fs::create_dir_all(&srv).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(srv.join("index.html"), NEEDLE).unwrap();
        Fixture { base, srv, home }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Connect (retrying while the netpoller reaches Accept), GET `/index.html`, and
/// return the response bytes. `alive` lets the caller abort early if the server died.
fn fetch(port: u16, alive: &dyn Fn() -> bool) -> Vec<u8> {
    let mut stream = None;
    for _ in 0..24000 {
        if !alive() {
            break;
        }
        if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
            stream = Some(s);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let mut s = stream.expect("caddy never accepted a connection");
    s.write_all(b"GET /index.html HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("send request");
    s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let mut resp = Vec::new();
    let _ = s.read_to_end(&mut resp);
    resp
}

fn assert_served(resp: &[u8]) {
    let text = String::from_utf8_lossy(resp);
    assert!(
        text.starts_with("HTTP/1.1 200 OK"),
        "expected 200 from caddy, got:\n{text}"
    );
    assert!(
        resp.windows(NEEDLE.len()).any(|w| w == NEEDLE),
        "served index.html body missing, got:\n{text}"
    );
}

/// Run `caddy file-server` under `backend` on the threaded driver, then fetch.
fn serve_and_fetch(backend: Box<dyn Backend>, tier: Option<u32>) -> Vec<u8> {
    let fx = Fixture::new("guest");
    let caddy = std::fs::read(caddy_path()).unwrap();
    let port = free_port();
    let (root, home) = (
        fx.srv.to_str().unwrap().to_string(),
        fx.home.to_str().unwrap().to_string(),
    );
    let (root2, home2) = (root.clone(), home.clone());
    let listen = format!(":{port}");

    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done2 = done.clone();
    let guest = thread::spawn(move || {
        let argv: [&[u8]; 7] = [
            b"caddy",
            b"file-server",
            b"--root",
            root.as_bytes(),
            b"--listen",
            listen.as_bytes(),
            b"--access-log",
        ];
        let home_env = format!("HOME={home}");
        let xdg_data = format!("XDG_DATA_HOME={home}");
        let xdg_cfg = format!("XDG_CONFIG_HOME={home}");
        let env: [&[u8]; 4] = [
            home_env.as_bytes(),
            xdg_data.as_bytes(),
            xdg_cfg.as_bytes(),
            b"GOMAXPROCS=2",
        ];
        Guest::new_static(&caddy)
            .reserved(GO_SPAN)
            .heap_base(HEAP_BASE)
            .brk_limit(BRK_LIMIT)
            .mmap_base(MMAP_BASE)
            .mmap_limit(MMAP_LIMIT)
            .stack_top(STACK_TOP)
            .argv(&argv)
            .env(&env)
            .tier_up(tier)
            .shim(move |s| {
                s.allow_dir(&root2);
                s.allow_write_dir(&home2);
            })
            .run_threaded(backend);
        done2.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let resp = fetch(port, &|| !done.load(std::sync::atomic::Ordering::SeqCst));
    // The server runs until process exit; the fetch used Connection: close, so caddy
    // keeps listening. Detach the guest thread — the test asserts on the response.
    drop(guest);
    resp
}

#[test]
fn caddy_serves_index_interp() {
    if !caddy_path().exists() {
        return skip_note();
    }
    assert_served(&serve_and_fetch(Box::new(InterpreterBackend), None));
}

/// Tiered JIT (FD-TIER, `tier_up(Some(50))`): caddy's startup-heavy cold code stays
/// interpreted while hot runtime loops compile.
#[test]
fn caddy_serves_index_jit() {
    if !caddy_path().exists() {
        return skip_note();
    }
    assert_served(&serve_and_fetch(Box::new(JitBackend::new()), Some(50)));
}

/// Native host control: the same binary serves on the host, proving the fixture and
/// the request/response expectation independent of the engine.
#[test]
fn caddy_serves_index_native() {
    if !caddy_path().exists() {
        return skip_note();
    }
    let fx = Fixture::new("native");
    let port = free_port();
    let mut child = std::process::Command::new(caddy_path())
        .args([
            "file-server",
            "--root",
            fx.srv.to_str().unwrap(),
            "--listen",
            &format!(":{port}"),
        ])
        .env("HOME", &fx.home)
        .env("XDG_DATA_HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.home)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn native caddy");
    let resp = fetch(port, &|| true); // native caddy boots in ~1s
    let _ = child.kill();
    let _ = child.wait();
    assert_served(&resp);
}
