// hello_go.elf — static Go (no cgo) acceptance fixture for the go-caddy track.
//
// Build (Go 1.26.3, reproducible, stripped):
//   CGO_ENABLED=0 GOOS=linux GOARCH=amd64 \
//     go build -trimpath -ldflags="-s -w" -o hello_go.elf hello_go.go
//
// Statically linked ET_EXEC. Writes to os.Stdout (fd 1) on purpose — Go's `println`
// builtin goes to stderr, which the threaded ProcOutcome doesn't yet capture (task-129).
// The PT_NOTE Go build-id note survives -s -w and drives has_go_build_note (P1b).
package main

import (
	"fmt"
	"os"
)

func main() {
	fmt.Fprintln(os.Stdout, "hello from go stdout")
}
