# Boot Process

## Panoramica

rustOS usa il **protocollo PVH**: il kernel è un ELF64 con una nota speciale
(`XEN_ELFNOTE_PHYS32_ENTRY`) che dice al loader di QEMU dove si trova il punto
di ingresso in protected mode a 32 bit. Niente GRUB, niente crate bootloader:
`qemu -kernel rustos-kernel` è tutto ciò che serve.

Il kernel è linkato **alto** (`-2G+1M`, ADR-0020) ma caricato **basso** (VMA ≠
LMA, `AT()` nel linker script): lo stub parte in PM 32-bit alle LMA, attiva
il paging su un PML4 dual-map (identity di transizione + mappa alta) e salta
in alto prima di `rust_main()`, che rimuove l'identity (`unmap_low()`).

```
1. QEMU (loader PVH) carica i segmenti ELF alle LMA (nota PVH: entry 0x100000)
2. trasferisce il controllo a 0x100000 in PM 32-bit flat
3. boot.asm: CR3 → PML4 dual-map, PAE + LME + PG, retf in 64-bit (LOW),
   salto assoluto HIGH, stack alto, call rust_main()
4. rust_main(): guard fail-loud a stadi → unmap_low() (PML4[0] = 0) → Rust
```

## I due attori

### 1. `boot.asm` — trampolino (~90 righe)

Esegue prima del passaggio a 64 bit, quindi deve restare assembly con encoding
32-bit garantito. Fa solo controllo hardware:

- stack di transizione (LOW, identity), CR3 → PML4 (LMA via alias linker:
  `R_X86_64_32` non contiene VMA alte, quindi `BOOT_PML4_LMA = BOOT_PML4 −
  OFFSET` valutato dal linker; la LMA del salto da EIP reale via `call/pop`)
- CR4: LA57 off + PAE on
- MSR EFER.LME ← 1
- LGDT (GDT di boot, base LMA: paging ancora off)
- CR0.PG ← 1, poi `retf` con frame (CS=code64, EIP=LMA di `low_entry`):
  un `jmp far ptr16:32` non può esprimere VMA alte
- in 64-bit a indirizzo LOW: `movabs` + `jmp` alla VMA alta, stack alto
  (`.bss`), `call rust_main(hvm_start_info)`

### 2. `boot_tables.rs` — tabelle come statiche Rust

Le page table e la GDT sono `static` const-valutate a compile-time:

```rust
#[repr(C, align(4096))]
#[link_section = ".pagetables.pml4")]
pub static BOOT_PML4: PageTable = { /* ... */ };
```

Il linker script le colloca a LMA fisse (7 tabelle base a `0x90000`–`0x96FFF`,
PD direct + PT VGA a 16M in `.tables_high`) con VMA alte (`VMA = LMA +
OFFSET`, verificata da gate-0 `readelf`); l'ordine nelle sezioni è
vincolante. Vantaggi: tipizzate (`u64` = entry a 64 bit garantito),
ispezionabili, testabili. Il PML4 contiene:

```
PML4[0]     → identity [0, 8M) di transizione (rimossa da unmap_low())
PML4[511]   → immagine kernel alta (finestra 16M a pagine 2M)
PML4[0x111] → direct map [0, 64G) a pagine 2M (+ split VGA UC a 4K)
```

## Dettagli della transizione

```
CR3  ← PML4 @0x90000 (LMA)
CR4  : LA57=0, PAE=1
EFER : LME=1          (via RDMSR/WRMSR, MSR 0xC0000080)
CR0  : PG=1           → long mode "compatibility" (codice ancora 32-bit!)
RETF 0x08:LMA(low_entry) → ricarica CS con L=1 → 64-bit a indirizzo LOW
JMP  rax (VMA alta)   → 64-bit in alto
RSP  ← BOOT_HIGH_STACK → CALL  rust_main(hvm_start_info in RDI)
```

Regole hardware rispettate:
- LME va scritto **prima** di PG (altrimenti #GP); LMA viene settata dall'hardware
- il bit L del code descriptor si cambia SOLO ricaricando CS (far jump/retf/iret)
- le pagine 2M vogliono basi pari: la VMA a `-2G+1M` rende lo scarto congruo
  (basi dispari = bit riservato → `#PF` RSVD a zero output, vedi ADR-0020)

A inizio `rust_main`, guard fail-loud a stadi su seriale raw (finestra
immagine, CR3 atteso): ogni indirizzo sbagliato nel flip è triple-fault
muto, i guard sono l'unica diagnostica.

## Funzionalità fornite dal loader PVH

| Funzionalità | Descrizione |
|--------------|-------------|
| Caricamento ELF64 | Segmenti PT_LOAD alle LMA (`p_paddr`), .bss azzerata |
| Entry PM 32-bit | Flat segments, paging off |
| hvm_start_info | Puntatore in EBX: memory map e820 inclusa (Fase 4) |

## Test

```bash
./run.sh                          # seriale su stdout, esce con Ctrl-C
RUN_DISPLAY=gtk ./run.sh          # VGA visibile in locale
```

Gate-0 statico prima di ogni boot dopo tocchi al layout (ADR-0020):
`readelf -l` (VMA−LMA == OFFSET su ogni PT_LOAD + nota PVH a 0x100000).

## Riferimenti

- [PVH boot protocol](https://xenbits.xen.org/docs/unstable/misc/pvh.html)
- [OSDev Wiki - Boot Process](https://wiki.osdev.org/Boot_Process)
- [ADR-0020: Higher-half kernel + direct map](./adr/0020-higher-half-direct-map.md)
