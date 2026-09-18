; boot.asm H1 — tutto-alto dual-map: PM32 (LMA) -> long mode (VMA) -> rust_main
;
; Il loader PVH di QEMU (-kernel ELF con nota XEN_ELFNOTE_PHYS32_ENTRY)
; carica il kernel alle LMA e trasferisce il controllo qui (_start, LMA 1M)
; in protected mode a 32 bit, paging off, segmenti flat.
;
; Le page table e la GDT sono statiche Rust const-valutate in
; src/boot_tables.rs (sezione .pagetables, LMA 0x90000). Il PML4 contiene sia
; l'identity di transizione [0, 8M) (PML4[0]) che la mappa alta (kernel +
; direct map): dopo `mov cr3` si salta HIGH e il basso resta solo fino a H2.
;
; Vincolo reloc 32-bit: in un oggetto elf64 `mov eax, SIMBOLO' emette
; R_X86_64_32, valida solo per valori < 4G — i simboli VMA alti non ci stanno.
; Lo stub usa quindi gli alias LMA definiti dal linker (`BOOT_PML4_LMA`,
; `BOOT_GDT_LMA`: valori < 4G garantiti) e, per il proprio salto, la LMA letta
; da EIP a runtime (`call/pop` + delta stessa-sezione, sempre piccolo).
; OFFSET resta in `linker.ld`/`addr.rs`: gate-0 readelf (VMA - LMA di ogni
; PT_LOAD) lo verifica prima di ogni boot H1.
;
; NB: le entry delle page table in long mode sono LARGHE 8 BYTE.

BITS 32
global _start
extern rust_main
extern BOOT_PML4_LMA
extern BOOT_GDT_LMA
extern BOOT_HIGH_STACK

; Stack di transizione (LOW, identity fino all'unmap H2).
STACK_TOP equ 0x0009F000

section .text.boot exec
_start:
    cli
    mov esp, STACK_TOP

    ; CR3 = LMA del PML4 di boot (alias linker, < 4G garantito).
    mov eax, BOOT_PML4_LMA
    mov cr3, eax

    mov eax, cr4
    and eax, ~(1 << 12)             ; LA57 off: walk a 4 livelli
    or  eax, 0x20                   ; PAE
    mov cr4, eax

    mov ecx, 0xC0000080             ; MSR EFER
    rdmsr
    or  eax, 1 << 8                 ; LME
    or  eax, 1 << 11                ; NXE (M1: PTE NX enforced; x86-64 lo
                                    ; richiede, QEMU/KVM lo supportano sempre)
    wrmsr

    ; Pseudo-descrittore LGDT sullo stack: limit noto, base = LMA della GDT
    ; (paging ancora off: linear == phys).
    sub esp, 8
    mov word [esp], 512*8 - 1
    mov eax, BOOT_GDT_LMA
    mov dword [esp + 2], eax
    lgdt [esp]
    add esp, 8

    ; Un jmp far ptr16:32 non puo' esprimere VMA alte: LMA di low_entry da
    ; EIP reale (call/pop) + delta stessa-sezione, poi retf in modo 64-bit.
    call next_eip
next_eip:
    pop eax                         ; LMA(next_eip)
    add eax, low_entry - next_eip    ; + delta -> LMA(low_entry)
    push dword 0x08
    push eax
    mov eax, 0x80000011             ; PE | ET | PG -> long mode compat
    mov cr0, eax
    retf                            ; CS=0x08 (L=1), EIP=LMA(low_entry)

[BITS 64]
low_entry:
    ; 64-bit a indirizzo LOW: salto assoluto (movabs) alla VMA alta.
    mov rax, high_entry
    jmp rax

high_entry:
    mov dx, 0x10                    ; selettore dati della GDT di boot
    mov ds, dx
    mov es, dx
    mov ss, dx
    ; Stack alto (.bss, VMA alta): da qui rust_main non tocca piu' il basso.
    ; H2 fa unmap di PML4[0] a inizio rust_main — lo stack LOW di transizione
    ; (STACK_TOP) resta valido solo per lo stub fin qui.
    lea rax, [rel BOOT_HIGH_STACK]
    add rax, 16384                  ; top (base 16-allineata, 16384 % 16 == 0)
    mov rsp, rax
    xor ebp, ebp

    mov edi, ebx                    ; arg1: hvm_start_info (indirizzo fisico,
                                    ; sopravvissuto ai cambi di modalita')
    call rust_main

.halt:
    cli
    hlt
    jmp .halt
