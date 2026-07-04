#include <unistd.h>
int main(void) { write(1, "hello musl\n", 11); return 0; }
