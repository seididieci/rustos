# ADR-0004: Boot via protocollo PVH con stub custom

> **Nota di superamento (Fase 27)**: tabelle e mappa di boot sono oggi
> higher-half + direct map (ADR-0020): `BOOT_MAP_LIMIT` non esiste piu'
> (finestra immagine statica + guard), le PD direct stanno a LMA 16M. Resta
> valido tutto il resto (PVH, stub, GDT statica, niente bootloader).

## Status

Accepted

## Context

Il piano originale prevedeva il crate `bootloader` 0.11 + `bootimage`. Durante
l'implementazione della Fase 1 sono emersi due blocchi:

1. **`bootloader` 0.11 e nightly recentissime**: il build script del crate non
   riesce più a compilare il suo componente UEFI (simbolo `wcslen` mancante al
   link con `rust-lld`), bloccando anche `cargo bootimage`.

2. **QEMU 10.x e loader alternativi**: sia il loader multiboot sia un finto
   bzImage si sono rivelati inaffidabili per il nostro caso; il kernel Linux
   reale invece boota correttamente sullo stesso QEMU tramite il protocollo
   PVH.

Inoltre il debugging ha portato alla luce un bug sottile nel nostro stub:
in long mode le entry delle page table sono **larghe 8 byte** — scriverle con
stride 4 produce walk verso indirizzi fisici inesistenti.

## Decision

- Il kernel è un **ELF64 con nota PVH** (`XEN_ELFNOTE_PHYS32_ENTRY`, type 18)
  aggiunta dal linker script nella sezione `.note`.
- QEMU lo carica direttamente con `-kernel` e trasferisce il controllo in
  **protected mode 32-bit flat** all'entry `_start` (0x100000).
- `kernel/src/boot.asm` (NASM) fa da trampolino:
  check CPUID → identity map di boot da 8 MiB (PD[1..3] large page 2 MiB;
  i 2 MiB originari non bastarono piu' dal Fase 17, quando il `.bss`
  supero' il limite → triple fault pre-IDT; `BOOT_MAP_LIMIT` + guard
  fail-loud a inizio `rust_main`) → PAE + LME +
  PG → far jump in 64-bit → `call rust_main`.
- Le tabelle vive in memoria convenzionale (`0x90000-0x93FFF`), fuori dalla
  zona del kernel.
- Niente GRUB, niente crate bootloader, niente multiboot.

## Consequences

### Positive

- Zero dipendenze esterne per il boot: tutto il percorso d'avvio è codice
  nostro e leggibile in una pagina.
- Immune ai cambi delle nightly e alle versioni di QEMU per quanto riguarda
  i loader.
- Percorso di boot interamente sotto controllo: ogni passo del caricamento è
  codice nostro, ispezionabile e debuggabile.

### Negative

- Non gira su hardware reale senza un loader PVH (GRUB 2.06+ lo supporta via
  modulo `pvhio`/multiboot2, o Xen). Per l'hardware vero si valuterà GRUB in
  una fase successiva.
- Il memory map di boot (e820) non arriva gratis come con multiboot: va letto
  via fw_cfg o INT 15h quando servirà (Fase 4).

### Neutral

- Lo stub assume entry in PM 32-bit flat: è ciò che garantisce il loader PVH.
- **Refactor successivo (stesso ADR)**: tabelle e GDT sono migrati a statiche
  Rust const-valutate (`src/boot_tables.rs`, sezione `.pagetables` linkata a
  indirizzi fissi con ordine vincolante). `boot.asm` è ridotto alle sole
  istruzioni di controllo hardware impossibili pre-switch; le entry a 64 bit
  sono ora garantite dal tipo.

## References

- [PVH boot protocol](https://xenbits.xen.org/docs/unstable/misc/pvh.html)
- QEMU `hw/i386/x86.c`: `load_elf_pvh()`
- ADR-0001 (nightly), ADR-0002 (bootloader crate, ora superseded da questa)
