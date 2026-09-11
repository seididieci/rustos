; boot.asm — trampolino minimo: protected mode 32-bit -> long mode -> rust_main
;
; Il loader PVH di QEMU (-kernel ELF64 con nota XEN_ELFNOTE_PHYS32_ENTRY)
; carica il kernel a 0x100000 e trasferisce il controllo qui in protected
; mode a 32 bit, paging off, segmenti flat.
;
; Le page table e la GDT NON vengono costruite qui: sono statiche Rust
; const-valutate in src/boot_tables.rs (sezione .pagetables @ 0x90000).
; Qui resta solo il controllo hardware che prima del far jump non puo'
; essere codice compilato a 64 bit.
;
; NB: le entry delle page table in long mode sono LARGHE 8 BYTE.

BITS 32
global _start
extern rust_main
extern BOOT_PML4
extern BOOT_GDT

STACK_TOP equ 0x0009F000

section .text.boot exec
_start:
    cli
    mov esp, STACK_TOP

    mov eax, BOOT_PML4              ; simbolo Rust -> indirizzo assoluto
    mov cr3, eax

    mov eax, cr4
    and eax, ~(1 << 12)             ; LA57 off: walk a 4 livelli
    or  eax, 0x20                   ; PAE
    mov cr4, eax

    mov ecx, 0xC0000080             ; MSR EFER
    rdmsr
    or  eax, 1 << 8                 ; LME
    wrmsr

    ; pseudo-descrittore LGDT costruito sullo stack:
    ;   limit = 512*8 - 1 (costante nota, vedi PageTable::LIMIT)
    ;   base  = indirizzo di BOOT_GDT (relocation assoluta)
    sub esp, 8
    mov word [esp], 512*8 - 1
    mov dword [esp + 2], BOOT_GDT
    lgdt [esp]
    add esp, 8

    mov eax, 0x80000011             ; PE | ET | PG -> long mode compat
    mov cr0, eax

    jmp 0x08:.long_mode_entry       ; ricarica CS (L=1): 64-bit vero

[BITS 64]
.long_mode_entry:
    mov dx, 0x10                    ; selettore dati della GDT di boot
    mov ds, dx
    mov es, dx
    mov ss, dx
    mov esp, STACK_TOP
    xor ebp, ebp

    mov edi, ebx                    ; arg1: hvm_start_info (indirizzo fisico,
                                    ; sopravvissuto ai cambi di modalita')
    call rust_main

.halt:
    cli
    hlt
    jmp .halt
