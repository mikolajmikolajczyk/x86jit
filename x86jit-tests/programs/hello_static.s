.intel_syntax noprefix
.global _start
.text
_start:
 mov rax,1
 mov rdi,1
 lea rsi,[rip+msg]
 mov rdx,6
 syscall
 mov rax,60
 xor rdi,rdi
 syscall
msg:
 .ascii "hello\n"
