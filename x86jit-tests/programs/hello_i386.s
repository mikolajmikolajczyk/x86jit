.intel_syntax noprefix
.code32
.global _start
.text
_start:
 mov eax,4          # __NR_write (i386)
 mov ebx,1          # fd = stdout
 lea ecx,[msg]
 mov edx,11         # len
 int 0x80
 mov eax,1          # __NR_exit (i386)
 xor ebx,ebx        # status = 0
 int 0x80
msg:
 .ascii "hello i386\n"
