// httpserve_go.elf — static Go net/http file-server fixture for go-caddy P5 (caddy
// endgame). Serves a single index.html over a real host-reachable TCP socket through
// the full net/http stack: this is the rung above tcpserve_go.elf (raw net), exercising
// http.Server request parsing, the http.FileServer static handler, and graceful
// Shutdown — the same surface caddy's file_server uses.
//
// Build (Go 1.26.3, reproducible, stripped):
//   CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
//     go build -trimpath -ldflags="-s -w" -o httpserve_go.elf httpserve.go
//
// Listens on 127.0.0.1:<argv[1]>, serves index.html via http.FileServerFS over an
// in-memory FS, then shuts down shortly after the first response so the guest exits.
package main

import (
	"context"
	"net"
	"net/http"
	"os"
	"sync/atomic"
	"testing/fstest"
	"time"
)

const index = "<!doctype html>\n<title>x86jit</title>\n<h1>hello from caddy-ish go</h1>\n"

func main() {
	if len(os.Args) < 2 {
		os.Exit(3)
	}
	ln, err := net.Listen("tcp", "127.0.0.1:"+os.Args[1])
	if err != nil {
		os.Exit(1)
	}
	fsys := fstest.MapFS{"index.html": {Data: []byte(index)}}
	files := http.FileServerFS(fsys)
	var served atomic.Bool
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		files.ServeHTTP(w, r)
		served.Store(true)
	})
	srv := &http.Server{Handler: handler}
	done := make(chan struct{})
	go func() {
		// After the first response lands, stop the server so the guest can exit.
		for range time.Tick(20 * time.Millisecond) {
			if served.Load() {
				// Shutdown drains active connections before returning, so the
				// response flush is sequenced before exit. Serve returns
				// ErrServerClosed the instant the listener closes (before the
				// drain), so exiting on Serve's return alone would race the
				// flush and truncate the response to an empty close — wait for
				// Shutdown to finish via `done` (the Go docs' explicit rule).
				_ = srv.Shutdown(context.Background())
				close(done)
				return
			}
		}
	}()
	_ = srv.Serve(ln)
	<-done
	os.Exit(0)
}
