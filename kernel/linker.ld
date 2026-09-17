OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

/* H1: VMA alte, LMA basse. OFFSET deve restare == addr.rs KERNEL_OFFSET
   (= 0xFFFFFFFF80000000: VMA = -2G+1M, LMA = 1M; lo scarto di 1M rende le
   entry PD 2M allineate, vedi addr.rs). Gate-0 `readelf -l`: VMA - LMA di
   ogni PT_LOAD. */
OFFSET = 0xFFFFFFFF80000000;

PHDRS {
    text   PT_LOAD FLAGS(5);          /* R-X */
    rodata PT_LOAD FLAGS(4);          /* R-- */
    data   PT_LOAD FLAGS(6);          /* RW- */
    note   PT_NOTE FLAGS(4);
    tables PT_LOAD FLAGS(6);          /* RW: page tables di transizione */
    tables_high PT_LOAD FLAGS(6);     /* RW: PD direct + PT VGA (LMA 16M) */
}

SECTIONS {
    /* Tabelle di boot (statiche Rust, vedi src/boot_tables.rs):
       LMA fissa in memoria convenzionale (CR3 phys), VMA alta.
       L'ORDINE e' vincolante: gli indirizzi LMA sono costanti in boot_tables.rs */
    .pagetables 0xFFFFFFFF80090000 : AT(0x90000) {
        KEEP(*(.pagetables.pml4))
        KEEP(*(.pagetables.pdpt))
        KEEP(*(.pagetables.pd))
        KEEP(*(.pagetables.pt))
        KEEP(*(.pagetables.pdpt_k))
        KEEP(*(.pagetables.pd_k))
        KEEP(*(.pagetables.pdpt_direct))
    } :tables

    /* PD direct + PT VGA a LMA FISSA 16M (sezione dedicata): sotto 1M non c'e'
       RAM oltre 0x9FC00 (buco PCI/VGA). VMA = LMA + OFFSET (regola uniforme,
       verificata da gate-0). 16M e' oltre heap+bitmap in ogni config. */
    .tables_high 0xFFFFFFFF81000000 : AT(0x1000000) {
        KEEP(*(.tables_high.pd_direct))
        KEEP(*(.tables_high.pt_vga))
    } :tables_high

    /* LMA 1M <-> VMA KERN_VIRT_BASE (VMA = LMA + OFFSET): l'immagine resta
       dov'era in fisico, cambia solo la vista virtuale. */
    . = 0xFFFFFFFF80100000;

    /* _kernel_start/_kernel_end: VMA alte (contabilita' frame via
       kern_virt_to_phys in phys_mem::init). */
    _kernel_start = .;

    .text : AT(ADDR(.text) - OFFSET) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    } :text

    .rodata : AT(ADDR(.rodata) - OFFSET) ALIGN(4K) {
        *(.rodata .rodata.*)
    } :rodata

    /* Nota PVH: dice al loader di QEMU l'indirizzo di ingresso in PM 32-bit.
       E' la LMA di _start (primo byte di .text): derivata, non hardcodata. */
    .note : ALIGN(4) {
        LONG(4)                       /* namesz: "Xen" + NUL */
        LONG(4)                       /* descsz */
        LONG(18)                      /* type = XEN_ELFNOTE_PHYS32_ENTRY */
        BYTE(0x58) BYTE(0x65) BYTE(0x6e) BYTE(0)   /* "Xen\0" */
        LONG(ADDR(.text) - OFFSET)    /* LMA di _start (QEMU carica i PT_LOAD a p_paddr) */
    } :note

    .data : AT(ADDR(.data) - OFFSET) ALIGN(4K) {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        /* Sezioni "orfane" del linker: senza posizionarle qui finirebbero
           DOPO _kernel_end e verrebbero sovrascritte dalla bitmap del frame
           allocator (bug: call indirette via GOT → salto a 0xFFFF...). */
        *(.got .got.*)
        *(.got.plt)
    } :data

    .bss : AT(ADDR(.bss) - OFFSET) ALIGN(4K) {
        *(COMMON)
        *(.bss .bss.*)
    } :data

    _kernel_end = ALIGN(4K);

    /* NB: questo rust-lld non accetta ASSERT (errore di parse anche su
       `ASSERT(1, "x")`): l'invariante VMA = LMA + OFFSET resta verificata da
       gate-0 `readelf` (vedi H1), non dal linker. */

    /* Alias LMA per lo stub PM32 (`boot.asm`): R_X86_64_32 accetta solo
       valori < 4G, quindi lo stub non puo' riferire i simboli VMA alti —
       usa questi (valutati qui, a 64 bit, poi troncati a 32). */
    BOOT_PML4_LMA = BOOT_PML4 - OFFSET;
    BOOT_GDT_LMA = BOOT_GDT - OFFSET;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
