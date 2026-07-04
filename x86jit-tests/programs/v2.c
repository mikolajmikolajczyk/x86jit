#include <nmmintrin.h>
#include <unistd.h>
typedef unsigned long u64; typedef unsigned int u32;
int main(void){
    volatile u64 vin = 0xF0F0F0F0F0F0F0F0UL;
    volatile u32 vc = 0x42;
    volatile long lanes[2] = {10, 3};
    volatile long bl[2] = {5, 7};
    long acc = 0;
    acc += __builtin_popcountll(vin);              // popcnt
    acc += __builtin_popcount((u32)vin);           // popcnt 32
    u32 crc = _mm_crc32_u8(0, vc);
    crc = _mm_crc32_u64(crc, vin);
    acc += (long)(crc & 0xFFF);
    __m128i v   = _mm_loadu_si128((__m128i*)&(char[16]){1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16});
    __m128i idx = _mm_loadu_si128((__m128i*)&(char[16]){15,14,13,12,11,10,9,8,7,6,5,4,3,2,1,0});
    __m128i r   = _mm_shuffle_epi8(v, idx);        // pshufb
    acc += _mm_extract_epi8(r, 0);                  // pextrb: 16
    __m128i a = _mm_loadu_si128((__m128i*)lanes), b = _mm_loadu_si128((__m128i*)bl);
    acc += _mm_movemask_epi8(_mm_cmpgt_epi64(a, b)); // pcmpgtq
    __m128i mul = _mm_mullo_epi32(_mm_set1_epi32((int)vc), _mm_set1_epi32(6)); // pmulld
    acc += _mm_extract_epi32(mul, 0);               // pextrd
    __m128i zz = _mm_cvtepu8_epi16(v);              // pmovzxbw
    acc += _mm_extract_epi16(zz, 1);
    char buf[32]; int p = 31; buf[p--] = '\n'; long x = acc;
    if (x == 0) buf[p--] = '0';
    while (x > 0) { buf[p--] = '0' + (int)(x % 10); x /= 10; }
    write(1, buf + p + 1, 31 - p);
    return 0;
}
