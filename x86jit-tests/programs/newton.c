// Newton's method for sqrt(2): pure double arithmetic, deterministic (IEEE-754).
// Exercises scalar SSE2 double: mul/sub/div, cvtsi2sd, cvttsd2si, comparisons.
typedef unsigned long u64;
static u64 slen(const char*s){u64 n=0;while(s[n])n++;return n;}
int main(void){
  double x = 1.0;
  for (int i = 0; i < 60; i++)
    x = x - (x*x - 2.0) / (2.0*x);
  // x ~ 1.41421356237... ; scale to integer nanounits, deterministic.
  long v = (long)(x * 1000000000.0);
  // format decimal into buf
  char buf[32]; int p = 31; buf[p--] = '\n';
  if (v == 0) buf[p--] = '0';
  while (v > 0) { buf[p--] = '0' + (int)(v % 10); v /= 10; }
  const char* s = buf + p + 1;
  __asm__ volatile("syscall" :: "a"(1),"D"(1),"S"(s),"d"(slen(s)) : "rcx","r11","memory");
  __asm__ volatile("syscall" :: "a"(60),"D"(0) : "rcx","r11","memory");
  return 0;
}
