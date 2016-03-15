format binary
use32

ERROR_LOAD_FAILED    = 1
ERROR_INIT_NOT_FOUND = 2
ERROR_INIT_FAILED    = 3

virtual at ebx
    module_path dd ?
    init_name   dd ?
    user_data   dd ?
    user_len    dd ?
    last_error  dd ?
end virtual

virtual at edi + functions
    LoadLibraryW   dd ?
    FreeLibrary    dd ?
    GetProcAddress dd ?
    GetLastError   dd ?
end virtual



start:
    push  ebx
    push  esi
    push  edi
    call  .get_eip
.get_eip:
    pop   eax
    lea   edi, [eax - (.get_eip - start)]
    mov   ebx, [esp + 16]
    push  [module_path]
    call  [LoadLibraryW]
    test  eax, eax
    jnz   .find_init
    call  [GetLastError]
    mov   [last_error], eax
    mov   eax, ERROR_LOAD_FAILED
    jmp   .end
.find_init:
    cmp   [init_name], 0
    je    .success
    mov   esi, eax
    push  [init_name]
    push  eax
    call  [GetProcAddress]
    test  eax, eax
    jnz   .call_init
    call  [GetLastError]
    mov   [last_error], eax
    push  esi
    call  [FreeLibrary]
    mov   eax, ERROR_INIT_NOT_FOUND
    jmp   .end
.call_init:
    lea   edx, [user_len]
    lea   ecx, [user_data]
    push  edx
    push  ecx
    call  eax
    add   esp, 8
    test  eax, eax
    jnz   .success
    push  esi
    call  [FreeLibrary]
    mov   eax, ERROR_INIT_FAILED
    jmp   .end
.success:
    xor   eax, eax
.end:
    pop   edi
    pop   esi
    pop   ebx
    ret   4

    align 4
functions: