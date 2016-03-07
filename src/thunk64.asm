format binary
use64

ERROR_LOAD_FAILED    = 1
ERROR_INIT_NOT_FOUND = 2
ERROR_INIT_FAILED    = 3

virtual at rbx
    module_path dq ?
    init_name   dq ?
    user_data   dq ?
    user_len    dq ?
    last_error  dd ?
end virtual

virtual at functions
    LoadLibraryW   dq ?
    FreeLibrary    dq ?
    GetProcAddress dq ?
    GetLastError   dq ?
end virtual



start:
    push  rbx
    push  rsi
    sub   rsp, 40
    mov   rbx, rcx
    mov   rcx, [module_path]
    call  [LoadLibraryW]
    test  rax, rax
    jnz   .find_init
    call  [GetLastError]
    mov   [last_error], eax
    mov   eax, ERROR_LOAD_FAILED
    jmp   .end
.find_init:
    cmp   [init_name], 0
    je    .success
    mov   rsi, rax
    mov   rdx, [init_name]
    mov   rcx, rax
    call  [GetProcAddress]
    test  rax, rax
    jnz   .call_init
    call  [GetLastError]
    mov   [last_error], eax
    mov   rcx, rsi
    call  [FreeLibrary]
    mov   eax, ERROR_INIT_NOT_FOUND
    jmp   .end
.call_init:
    lea   rdx, [user_len]
    lea   rcx, [user_data]
    call  rax
    test  al, 1
    jnz   .success
    mov   rcx, rsi
    call  [FreeLibrary]
    mov   eax, ERROR_INIT_FAILED
    jmp   .end
.success:
    xor   eax, eax
.end:
    add   rsp, 40
    pop   rsi
    pop   rbx
    ret

    align 8
functions: