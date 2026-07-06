// Freestanding single-connection TCP server: binds 127.0.0.1:<argv[1]>, accepts
// one client, reads its request (discarded), writes a fixed HTTP/1.1 200 response,
// then exits. No libc, no fork, no threads — pure raw syscalls, so it runs on the
// current x86jit instruction set and exercises exactly the blocking-socket surface
// the shim gained in Phase 0 (socket/setsockopt/bind/listen/accept + socket
// read/write/close). Built with -nostdlib -static -no-pie (ET_EXEC at 0x400000).
//
//   gcc -nostdlib -static -no-pie -fno-stack-protector -O2 \
//       -o tcpserve.elf tcpserve.c
//
// The port is argv[1] (a free port the test picks and passes in) so parallel test
// runs never collide on a fixed port.

typedef unsigned char u8;

// x86-64 syscall numbers used here.
enum {
    SYS_read = 0,
    SYS_write = 1,
    SYS_close = 3,
    SYS_socket = 41,
    SYS_accept = 43,
    SYS_bind = 49,
    SYS_listen = 50,
    SYS_setsockopt = 54,
    SYS_exit = 60,
};

static long sys6(long n, long a, long b, long c, long d, long e) {
    long r;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8)
                     : "rcx", "r11", "memory");
    return r;
}

static int atoi_(const char *s) {
    int v = 0;
    while (*s >= '0' && *s <= '9') {
        v = v * 10 + (*s - '0');
        s++;
    }
    return v;
}

// The body ("Served by x86jit\n") is asserted verbatim by the test after the
// header/body split.
static const char RESP[] =
    "HTTP/1.1 200 OK\r\n"
    "Content-Length: 17\r\n"
    "Connection: close\r\n"
    "\r\n"
    "Served by x86jit\n";

// Called by the naked `_start` below with (argc, argv). Never returns.
void server(long argc, char **argv) {
    int port = (argc > 1) ? atoi_(argv[1]) : 8080;

    long s = sys6(SYS_socket, 2 /*AF_INET*/, 1 /*SOCK_STREAM*/, 0, 0, 0);

    int one = 1;
    sys6(SYS_setsockopt, s, 1 /*SOL_SOCKET*/, 2 /*SO_REUSEADDR*/, (long)&one, 4);

    // struct sockaddr_in, 16 bytes: family(2, LE), port(2, big-endian), addr(4),
    // 8 bytes zero. 127.0.0.1 is the byte sequence {127,0,0,1}.
    u8 sa[16] = {0};
    sa[0] = 2; // AF_INET (little-endian u16)
    sa[2] = (u8)((port >> 8) & 0xff);
    sa[3] = (u8)(port & 0xff);
    sa[4] = 127;
    sa[5] = 0;
    sa[6] = 0;
    sa[7] = 1;
    sys6(SYS_bind, s, (long)sa, 16, 0, 0);
    sys6(SYS_listen, s, 1, 0, 0, 0);

    long c = sys6(SYS_accept, s, 0, 0, 0, 0);

    char buf[1024];
    sys6(SYS_read, c, (long)buf, sizeof(buf), 0, 0); // request, discarded

    sys6(SYS_write, c, (long)RESP, sizeof(RESP) - 1, 0, 0);
    sys6(SYS_close, c, 0, 0, 0, 0);
    sys6(SYS_close, s, 0, 0, 0, 0);
    sys6(SYS_exit, 0, 0, 0, 0, 0);
    __builtin_unreachable();
}

// Entry: read argc/argv off the initial stack, align, and tail into `server`.
__attribute__((naked, used)) void _start(void) {
    __asm__ volatile("xor %rbp, %rbp\n"
                     "mov (%rsp), %rdi\n"  // argc
                     "lea 8(%rsp), %rsi\n" // argv
                     "and $-16, %rsp\n"
                     "call server\n"
                     "mov $60, %rax\n" // exit(0) if server ever returns
                     "xor %rdi, %rdi\n"
                     "syscall\n");
}
