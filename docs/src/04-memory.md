# Memory Management

## Panoramica

Il gestore della memoria ha tre strati fondamentali (Fase 4) + il flip
higher-half (ADR-0020, H0/H1/H2):

1. **Kernel alto + direct map** — kernel a `-2G+1M`
   (`0xFFFF_FFFF_8010_0000`, LMA 1M), direct map di tutta la RAM a
   `0xFFFF_8880_0000_0000` con pagine 2M (statiche, 64G; fail-loud oltre)
2. **Physical frame allocator** — bitmap dimensionata a runtime dalla memory map, 1 frame = 4 KiB
3. **Kernel heap** — `linked_list_allocator` come `#[global_allocator]`

```
Ordine di inizializzazione:
  guard H1           → finestra immagine + pagine 1G (no: 2M, sempre) + CR3
  unmap_low()        → PML4[0] = 0: da qui solo alto + direct map
  boot_info::memmap()→ legge le regioni fisiche da hvm_start_info (via direct)
  max_addr           → tetto RAM = max(addr + size) delle regioni MEM_RAM
  vmm::init(max_addr)→ verifica tetto 64G (fail-loud oltre, niente top-up)
  phys_mem::init(...)→ bitmap allocator dalla memory map (+ riserva tabelle)
  heap::init(bitmap_end) → heap subito dopo kernel + bitmap
```

## Higher-half + direct map (ADR-0020)

Lo stub (`boot.asm`, tutto RIP-relative + alias LMA) carica CR3 con un PML4
dual-map: identity di transizione `[0, 8M)` (PML4[0], rimossa a runtime) +
immagine alta + direct map. Dopo il jump-high lo stack commuta su
`BOOT_HIGH_STACK` (.bss) e `rust_main` fa `unmap_low()` (`PML4[0] = 0` +
flush): da li' un NULL-deref faulta invece di leggere spazzatura. I PML4
user (creati dopo) ereditano il PML4 pulito per copia.

```
PML4[0]     → PDPT low  → identity [0, 8M) di transizione (solo boot)
PML4[511]   → PDPT_K    → PD_K: VMA [KERN-1M, +16M) → phys [0, 16M), pagine 2M
PML4[0x111] → PDPT_DIRECT → 32 PD: phys [0, 64G), pagine 2M
                           └─ PD[0][0] → PT_VGA: primi 2M a 4K, pagina VGA UC
```

Dettagli che hanno morso (ADR-0020 §dettagli): basi PD 2M pari (VMA −2G+1M,
stile Linux — basi dispari = bit riservato → `#PF` RSVD a zero output);
sotto 1M solo `0x90000–0x9FC00` e' RAM (buco PCI/VGA: le PD direct stanno a
LMA fissa 16M, verificata RAM fail-loud); pagine 2M e non 1G (baseline su
ogni x86-64: niente PDPE1GB, niente flag QEMU); tetto statico 64G fail-loud
oltre (config test ≤ 32G); VGA UC via split 4K (PAT di reset).

La pagina scratch dei test (`MAP_TEST_PHYS`, 64M) sta altrove per invariante
compilata: scriverci sopra le PD direct fu il fault ritardato di H2.

## Physical Frame Allocator — Bitmap dinamica

Ogni frame fisico di 4 KiB è rappresentato da 1 bit in una bitmap
posizionata **subito dopo `_kernel_end`** e dimensionata a runtime:

```
N frame = max_addr / 4096            (da memory map)
Bitmap  = N frame / 8 byte           (es. 5 GiB → 1.3 MiB di bitmap)
```

Init:
1. Tutti i frame marcati usati (bitmap = 0xFF)
2. Le regioni `MEM_RAM` vengono liberate
3. Kernel (VMA alte, contabilita' in PHYS via `kern_virt_to_phys`), tabelle
   base (0x90000–0x100000), tabelle alte 16M (verificate RAM, fail-loud),
   bitmap stessa, scratch test 64M e VGA buffer (0xB8000) vengono ri-marcati
   usati

API:
- `alloc() -> Option<u64>` — trova e marca un frame libero
- `free(frame)` — libera un frame
- `free_frames()` / `used_frames()` — statistiche
- `bitmap_end()` — fine bitmap (page-aligned), usata per posizionare l'heap

## Kernel Heap

Posizionato subito dopo kernel + bitmap (`bitmap_end()`), 4 MiB iniziali.
Usa `linked_list_allocator::LockedHeap` come `#[global_allocator]`.

Dopo `heap::init()`:
```rust
let v = alloc::vec![1, 2, 3];     // funziona!
let b = alloc::boxed::Box::new(42); // funziona!
```

`#[alloc_error_handler]` gestisce l'out-of-memory con panic.

## Layout fisico

```
0x00000 ─ 0x8FFFF   riservato (BIOS/IVT)
0x90000 ─ 0x96FFF   tabelle base boot (PML4/PDPT/PD/PT low + PDPT_K/PD_K/PDPT_DIRECT)
0x97000 ─ 0x9FBFF   RAM convenzionale libera (sotto il buco PCI/VGA)
0xA0000 ─ 0xFFFFF   buco PCI/VGA (NON RAM: letture 0xFF — mai tabelle qui!)
0x100000 ─ ─ ─ ─    kernel (text/rodata/data/bss, LMA; VMA a -2G+1M)
0x100000+  bitmap frame allocator (dim. variabile)
+          kernel heap (4 MiB)
+          RAM libera (frame allocator)
0x1000000 ─ 0x1021FFF tabelle alte (32 PD direct + PT VGA, LMA fissa 16M)
0x4000000  pagina scratch test MAP_TEST_PHYS (1 frame, oltre tutto)
```

> Nota: gli indirizzi esatti di bitmap/heap dipendono da `_kernel_end`
> e dalla quantità di RAM (bitmap più grande = heap più in alto).

## Errori comuni

| Errore | Causa | Soluzione |
|--------|-------|-----------|
| Page Fault | Accesso a pagina oltre il tetto mappa | Verificare `vmm::mapped_max()` |
| Out of Memory | Heap esaurito | `alloc_error_handler` → panic |
| Copertura insufficiente | RAM > 64 GiB | Alzare le PD statiche (meccanico, vedi ADR-0020) |
| #PF RSVD a zero output | Base PD 2M dispari (VMA/LMA incongrue) | Vedi ADR-0020 §1 + `const assert` in `addr.rs` |
| Tabelle illeggibili | LMA nel buco PCI/VGA (< 1M oltre 0x9FC00) | Solo 0x90000–0x96FFF sotto 1M; resto a 16M |

## Higher-half (fatto, ADR-0020)

Il **kernel higher-half** e' atterrato in H0/H1/H2 (vedi sopra + ADR-0020):
kernel a `-2G+1M`, direct map 2M, `PML4[0] = 0` a runtime. La protezione U/S
resta (pagine kernel supervisor-only), ma il basso canonico e' ora libero:
NULL-deref faulta, lo spazio user basso e' pulito per futuri mmap/brk.

Nota storica: era rimandato dalla Fase 6 (costo alto, benefici prematuri);
la condizione ("processi user che richiedono spazio basso pulito") e' maturata
con i servizi da disco e gli helper `spawn_image` (Fase 21).

## mmap anonimo nel basso canonico (Fase M0)

Il payoff dell'higher-half: il basso canonico (`[0x10_0000, 0x4000_0000)`,
1M–1G; i primi 64K mai assegnati → NULL faulta) ospita mappe anonime private
con zero-fill lazy (stesso contratto di `sbrk`: VA subito, frame al fault).

- Tabella VMA per-pid (16 record statici, mai heap — anche il fault handler
  fa lookup qui); overlap-check totale (zona/heap/stack/ring/altre VMA).
- Pagine materializzate `OWNED` → il teardown esistente le libera gratis;
  `munmap` solo su VMA intere (two-phase: valida tutto, poi muta).
- `is_user_range` esteso alle VMA vive: ogni syscall con buffer user
  (spawn, write, …) accetta memoria mappata senza cambi puntuali.
- Solo RW in M0 (`prot` diverso = `-1`); niente split, niente file-backed
  (page-in su fault verso userfs e' deadlock-prone: sua fase propria).
- `libr::mmap` / `mmap_fixed` / `munmap`; `sbrk`/heap/scratch invariati.

## Protezioni di memoria (Fase M1)

- Ogni VMA ha un `prot` (`PROT_NONE`/`PROT_READ`/`PROT_READ|PROT_WRITE`);
  `mmap` lo applica alla materializzazione, `mprotect` (syscall 41) lo cambia
  su VMA intere (RO↔RW flippa il bit W; NONE smappa+libera, riuso a zeri).
- EFER.NXE abilitato a boot: heap, stack, `mmap` e pagine iniettate sono
  non-eseguibili. Il binario user e' ancora RWX perche' flat (codice + dati in
  un'unica regione copiata): il W^X richiede i confini `.text`/`.data`
  all'embed-time (M1b).
- Il page-fault handler distingue: protection-violation da USER MODE (write su
  RO, exec su NX, accesso a NONE) o fault fuori regione (guard page sotto lo
  stack) → **kill del processo** (`FAULT_EXIT_CODE` 139, mai halt del kernel);
  fault supervisor → bug del kernel, halt. La guard page sta a
  `USER_STACK_GUARD` (pagina sotto lo stack, mai mappata).
- Estrazione del phys da una PTE SEMPRE con `PTE_ADDR_MASK` (bit 12..51): mai
  `& !0xFFF`, che con NX lascerebbe il bit 63 e corromperebbe il frame address.

## Riferimenti

- [Writing an OS in Rust - Heap Allocation](https://os.phil-opp.com/heap-allocation/)
- [OSDev Wiki - Bitmap Allocator](https://wiki.osdev.org/Bitmap_allocator)
- [OSDev Wiki - Paging](https://wiki.osdev.org/Paging)
- [Intel SDM - Chapter 4: Paging](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)
