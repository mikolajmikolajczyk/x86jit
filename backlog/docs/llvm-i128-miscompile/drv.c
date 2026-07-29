#include <stdio.h>
#include <string.h>
/* r = (xmm[a] & ~m) | (xmm[src] & m),  m = 0xFFFFFFFF ; stored to xmm[dst] */
void f(void *xmm, unsigned char dst, unsigned char a, unsigned char src, _Bool isf64);
int main(void) {
    static __attribute__((aligned(16))) unsigned long long q[32*2];
    memset(q, 0, sizeof q);
    q[6*2+0] = 0x000000003fb6cd8eULL; q[6*2+1] = 0x1111111111111111ULL; /* src */
    q[4*2+0] = 0xBBBBBBBBBBBBBBBBULL; q[4*2+1] = 0xAAAAAAAAAAAAAAAAULL; /* a   */
    q[2*2+0] = 0x2222222211111111ULL; q[2*2+1] = 0x444444443fb6cd8eULL; /* dst */
    f(q, 2, 4, 6, 0);
    unsigned long long want_hi = 0xAAAAAAAAAAAAAAAAULL;              /* a's high 64 */
    unsigned long long want_lo = 0xBBBBBBBB3fb6cd8eULL;              /* a hi32 | src lo32 */
    printf("got  xmm2 = %016llx %016llx\n", q[2*2+1], q[2*2+0]);
    printf("want xmm2 = %016llx %016llx\n", want_hi, want_lo);
    int bad = (q[2*2+1] != want_hi) || (q[2*2+0] != want_lo);
    puts(bad ? "MISCOMPILED" : "correct");
    return bad;
}
