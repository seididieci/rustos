/* linker script del uptime user: carica il codice a USER_CODE (0x4000_0000_0000),
   l'indirizzo virtuale dove il kernel mappa il binario nello spazio del
   processo (vmm_user::USER_CODE). niente entry (lo spawn set RIP = USER_CODE). */

OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

SECTIONS {
    . = 0x400000000000;

    .text : {
        /* _start DEVE stare all'inizio: il kernel salta a USER_CODE (= inizio
           di .text) come RIP iniziale. Senza questo KEEP, lld puo' mettere
           altre funzioni (es. rust_begin_unwind) PRIMA di _start e il boot
           eseguirebbe l'entry sbagliata (panic). niente_mangle → sezione
           .text._start. */
        KEEP(*(.text._start))
        *(.text .text.*)
    }

    .rodata : ALIGN(4K) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K) {
        *(.data .data.*)
    }

    .bss : ALIGN(4K) {
        *(COMMON)
        *(.bss .bss.*)
    }

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
