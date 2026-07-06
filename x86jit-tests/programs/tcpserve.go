// tcpserve_go.elf — static Go net server fixture for the go-caddy P4 netpoller test.
//
// Build (Go 1.26.3, reproducible, stripped):
//   CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
//     go build -trimpath -ldflags="-s -w" -o tcpserve_go.elf tcpserve.go
//
// Listens on 127.0.0.1:<argv[1]> (Go's net.Listen sets SO_REUSEADDR), accepts one
// connection, reads the request, writes a fixed HTTP/1.1 200, closes, exits. Exercises
// the full netpoller: epoll_create1/ctl/pwait, eventfd, nonblocking accept4/read/write.
package main

import (
	"net"
	"os"
)

func main() {
	if len(os.Args) < 2 {
		os.Exit(3)
	}
	ln, err := net.Listen("tcp", "127.0.0.1:"+os.Args[1])
	if err != nil {
		os.Exit(1)
	}
	conn, err := ln.Accept()
	if err != nil {
		os.Exit(2)
	}
	buf := make([]byte, 1024)
	_, _ = conn.Read(buf)
	_, _ = conn.Write([]byte("HTTP/1.1 200 OK\r\n\r\nhello from go\n"))
	_ = conn.Close()
	os.Exit(0)
}
