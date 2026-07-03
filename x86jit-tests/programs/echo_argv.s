.intel_syntax noprefix
.global _start
.text
_start:
    mov rsi, [rsp+16]        # argv[1] pointer
    xor rdx, rdx             # len = 0
strlen:
    mov al, [rsi+rdx]        # byte of argv[1]
    test al, al
    jz done
    inc rdx
    jmp strlen
done:
    mov rax, 1               # sys_write
    mov rdi, 1               # stdout
    syscall                  # write(1, argv[1], len)
    mov rdi, [rsp]           # argc
    mov rax, 60              # sys_exit
    syscall
