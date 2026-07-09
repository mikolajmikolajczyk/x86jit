//! A real web server serves a page on x86jit. busybox `httpd` in inetd mode
//! (`-i`) is a complete HTTP server: it reads the request from stdin, resolves the
//! path under its document root, and writes the HTTP response to stdout. Running it
//! on the busybox:musl image (which already runs three ways) serves `index.html`
//! from the rootfs as an `HTTP/1.1 200 OK` — under both the interpreter and the JIT,
//! byte-for-byte identical.
//!
//! This is the achievable "web server on x86jit" (see backlog/docs/design/go-runtime-gap.md):
//! it needs no networking syscalls (inetd hands the connection over stdin/stdout)
//! and no threads — only fork-free single-request handling. Exercises the chdir
//! (`-h /`) and graceful-ENOSYS fallbacks (getsockname/setsockopt/sendfile) the
//! shim added for it.

mod common;
use common::{oci, BUSYBOX_MUSL};

const PAGE: &[u8] = b"<html><body><h1>Served by x86jit</h1></body></html>\n";

#[test]
fn busybox_httpd_serves_index_three_ways() {
    let Some(ran) = oci(BUSYBOX_MUSL, "httpd-index")
        .file("index.html", PAGE)
        .argv(&["/bin/busybox", "httpd", "-i", "-h", "/"])
        .stdin(b"GET /index.html HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .expect_exit(0)
        .run()
    else {
        return;
    };

    let out = ran.stdout();
    let text = String::from_utf8_lossy(out);
    assert!(
        text.starts_with("HTTP/1.1 200 OK"),
        "expected a 200 response, got:\n{text}"
    );
    // The response body is the served file verbatim (after the header/body split).
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("HTTP header terminator");
    assert_eq!(&out[split + 4..], PAGE, "served body must equal index.html");
}
