OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

PHDRS {
    text   PT_LOAD FLAGS(5);          /* R-X */
    rodata PT_LOAD FLAGS(4);          /* R-- */
    data   PT_LOAD FLAGS(6);          /* RW- */
    note   PT_NOTE FLAGS(4);
    tables PT_LOAD FLAGS(6);          /* RW: page tables + GDT */
}

SECTIONS {
    /* Tabelle di boot (statiche Rust, vedi src/boot_tables.rs):
       memoria convenzionale sotto 1 MiB, lontane da kernel e stack.
       L'ORDINE e' vincolante: gli indirizzi sono costanti in boot_tables.rs */
    .pagetables 0x90000 : {
        KEEP(*(.pagetables.pml4))
        KEEP(*(.pagetables.pdpt))
        KEEP(*(.pagetables.pd))
        KEEP(*(.pagetables.pt))
    } :tables

    . = 1M;

    /* _kernel_start/_kernel_end: usati dal frame allocator per marcare
       i frame del kernel come occupati. */
    _kernel_start = .;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    } :text

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    } :rodata

    /* Nota PVH: dice al loader di QEMU l'indirizzo di ingresso in PM 32-bit */
    .note : ALIGN(4) {
        LONG(4)                       /* namesz: "Xen" + NUL */
        LONG(4)                       /* descsz */
        LONG(18)                      /* type = XEN_ELFNOTE_PHYS32_ENTRY */
        BYTE(0x58) BYTE(0x65) BYTE(0x6e) BYTE(0)   /* "Xen\0" */
        LONG(0x00100000)              /* punto di ingresso fisico (_start) */
    } :note

    .data : ALIGN(4K) {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        /* Sezioni "orfane" del linker: senza posizionarle qui finirebbero
           DOPO _kernel_end e verrebbero sovrascritte dalla bitmap del frame
           allocator (bug: call indirette via GOT → salto a 0xFFFF...). */
        *(.got .got.*)
        *(.got.plt)
    } :data

    .bss : ALIGN(4K) {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    _kernel_end = ALIGN(4K);

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
