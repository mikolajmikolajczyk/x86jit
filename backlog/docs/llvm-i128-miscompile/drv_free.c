/* Freestanding driver: no libc. Exit code 0 = correct, 1 = miscompiled. */
typedef unsigned long long u64;
void f(void *xmm, unsigned char dst, unsigned char a, unsigned char src, _Bool isf64);
static u64 q[64] __attribute__((aligned(16)));
static void sys_exit(long c) {
#if defined(__aarch64__)
    register long x8 __asm__("x8") = 93; register long x0 __asm__("x0") = c;
    __asm__ volatile("svc 0" :: "r"(x8), "r"(x0) : "memory");
#else
    register long rax __asm__("rax") = 60; register long rdi __asm__("rdi") = c;
    __asm__ volatile("syscall" :: "r"(rax), "r"(rdi) : "memory");
#endif
    __builtin_unreachable();
}
void _start(void) {
    for (int i = 0; i < 64; i++) q[i] = 0;
    q[12] = 0x000000003fb6cd8eULL; q[13] = 0x1111111111111111ULL; /* src  */
    q[8]  = 0xBBBBBBBBBBBBBBBBULL; q[9]  = 0xAAAAAAAAAAAAAAAAULL; /* a    */
    q[4]  = 0x2222222211111111ULL; q[5]  = 0x444444443fb6cd8eULL; /* dst  */
    f(q, 2, 4, 6, 0);
    sys_exit((q[5] != 0xAAAAAAAAAAAAAAAAULL || q[4] != 0xBBBBBBBB3fb6cd8eULL) ? 1 : 0);
}
