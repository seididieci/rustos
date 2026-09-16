# Boot Process

## Panoramica

Velordor usa il **protocollo PVH**: il kernel è un ELF64 con una nota speciale
(`XEN_ELFNOTE_PHYS32_ENTRY`) che dice al loader di QEMU dove si trova il punto
di ingresso in protected mode a 32 bit. Niente GRUB, niente crate bootloader:
`qemu -kernel velordor-kernel` è tutto ciò che serve.

```
1. QEMU (loader PVH) carica i segmenti ELF in RAM
2. trasferisce il controllo a 0x100000 in PM 32-bit flat
3. boot.asm: attiva long mode (identity map 8 MiB: PD[1..3] large page —
   servono dal Fase 17, quando il .bss supero' i 2 MiB originari)
4. call rust_main() — da qui in poi siamo in Rust 64-bit
```

## I due attori

### 1. `boot.asm` — trampolino (~50 righe)

Esegue prima del passaggio a 64 bit, quindi deve restare assembly con encoding
32-bit garantito. Fa solo controllo hardware:

- stack, CR3 → PML4
- CR4: LA57 off + PAE on
- MSR EFER.LME ← 1
- LGDT (GDT di boot)
- CR0.PG ← 1 (long mode compat) + far jump al code segment L=1

### 2. `boot_tables.rs` — tabelle come statiche Rust

Le page table e la GDT sono `static` const-valutate a compile-time:

```rust
#[repr(C, align(4096))]
#[link_section = ".pagetables.pt"]
pub static BOOT_PT: PageTable = { /* entry[i] = (i<<12)|PRESENT|WRITABLE */ };
```

Il linker script le colloca a indirizzi fissi (`0x90000`–`0x93FFF`, ordine
vincolante nel script!) e l'assembly vi accede via simboli. Vantaggi:
tipizzate (`u64` = entry a 64 bit garantito), ispezionabili, testabili.

## Dettagli della transizione

```
CR3  ← PML4 @0x90000
CR4  : LA57=0, PAE=1
EFER : LME=1          (via RDMSR/WRMSR, MSR 0xC0000080)
CR0  : PG=1           → long mode "compatibility" (codice ancora 32-bit!)
JMP  0x08:entry64     → ricarica CS con L=1 → 64-bit vero
CALL  rust_main(magic, mbi)
```

Regole hardware rispettate:
- LME va scritto **prima** di PG (altrimenti #GP); LMA viene settata dall'hardware
- il bit L del code descriptor si cambia SOLO ricaricando CS (far jump/retf/iret)

## Funzionalità fornite dal loader PVH

| Funzionalità | Descrizione |
|--------------|-------------|
| Caricamento ELF64 | Segmenti PT_LOAD a indirizzi fisici, .bss azzerata |
| Entry PM 32-bit | Flat segments, paging off |
| hvm_start_info | Puntatore in EBX: memory map e820 inclusa (Fase 4) |

## Test

```bash
./run.sh                          # seriale su stdout, esce con Ctrl-C
RUN_DISPLAY=gtk ./run.sh          # VGA visibile in locale
```

## Riferimenti

- [PVH boot protocol](https://xenbits.xen.org/docs/unstable/misc/pvh.html)
- [OSDev Wiki - Boot Process](https://wiki.osdev.org/Boot_Process)
