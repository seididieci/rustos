# Memory Management

## Panoramica

La Fase 4 implementa i tre strati fondamentali del gestore della memoria:

1. **Identity map dinamica** — copre tutta la RAM fino a ~109 GiB con large pages (2 MiB)
2. **Physical frame allocator** — bitmap dimensionata a runtime dalla memory map, 1 frame = 4 KiB
3. **Kernel heap** — `linked_list_allocator` come `#[global_allocator]`

```
Ordine di inizializzazione:
  boot_info::memmap()  → legge le regioni fisiche da hvm_start_info
  max_addr             → tetto RAM = max(addr + size) delle regioni MEM_RAM
  vmm::init(max_addr)  → estende identity map dinamicamente
  phys_mem::init(...)  → bitmap allocator dalla memory map
  heap::init(bitmap_end) → heap subito dopo kernel + bitmap
```

## Identity Map — 2 MiB Large Pages, dinamica

Le page table di boot (0x90000-0x93FFF) mappano già i primi 1 GiB:
`PML4[0] → PDPT[0] → PD[0] (0x92000)`, dove il PD[0] ha la entry 0 →
PT (primi 2 MiB con pagine da 4 KiB) e le entry 1-511 → large pages.

A runtime la mappa viene **estesa dinamicamente** in base a `max_addr`:

- **PDPT[0] → PD[0]** copre 0 – 1 GiB (entry 1-511 come large page 2 MiB).
- **PDPT[1..N] → PD@0x94000 + (i-1)×0x1000**: ogni PD aggiuntiva mappa
  512 entry × 2 MiB = 1 GiB, riempita fino a `max_addr`.

Le PD aggiuntive sono piazzate nello spazio riservato `0x94000 – 0xFFFFF`
(432 KiB = 108 PD), quindi la copertura massima è:

```
1 GiB (PD[0]) + 108 GiB (PD[1..108]) ≈ 109 GiB
```

```
PML4[0] → PDPT[0] → PD[0]@0x92000      → 0 – 1 GiB
                  → PD[1]@0x94000      → 1 – 2 GiB
                  → PD[2]@0x95000      → 2 – 3 GiB
                  → ...
                  → PD[108]@0x100000   → 108 – 109 GiB
```

Dopo l'estensione, un flush CR3 invalida la TLB.

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
3. Kernel, page tables + PD (0x90000–0x100000), bitmap stessa e VGA
   buffer (0xB8000) vengono ri-marcati usati

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

## Layout fisico (post-Fase 4)

```
0x00000 ─ 0x8FFFF   riservato (BIOS/IVT)
0x90000 ─ 0x93FFF   page tables boot (PML4/PDPT/PD/PT)
0x94000 ─ 0xFFFFF   PD aggiuntive dinamiche (108 × 4 KiB)
0x100000 ─ ─ ─ ─    kernel (text/rodata/data/bss)
0x100000+   bitmap frame allocator (dim. variabile)
+          kernel heap (4 MiB)
+          RAM libera (frame allocator)
```

> Nota: gli indirizzi esatti di bitmap/heap dipendono da `_kernel_end`
> e dalla quantità di RAM (bitmap più grande = heap più in alto).

## Errori comuni

| Errore | Causa | Soluzione |
|--------|-------|-----------|
| Page Fault | Accesso a pagina oltre il tetto mappa | Verificare `vmm::mapped_max()` |
| Out of Memory | Heap esaurito | `alloc_error_handler` → panic |
| Copertura insufficiente | RAM > 109 GiB | Servono PT a 4 KiB o higher-half |
| PD sovrapposte | `covered` avanzato di < 1 GiB per PD | Ogni PD deve aggiungere esattamente 1 GiB |

## Higher-half (rimandato)

Il **kernel higher-half** (mappare il kernel nella meta' alta dello spazio
virtuale, es. a partire da ~0xFFFF8000_00000000, lasciando la meta' bassa
pulita per lo user space) e' **rimandato a una fase futura**: il kernel resta
in identity map (bassa) con protezione U/S via bit delle PTE (ADR-0005).

Motivazione del rinvio:
- il costo e' alto (rilocazione del linker, boot con pagine higher-half,
  refactor del VMM, tutti i puntatori `&'static` del kernel vengono toccati);
- i benefici (spazio utente basso pulito e separato) servono quando ci saranno
  processi user reali e concorrenti (init, console server), non all'MVP della
  Fase 6;
- prima di arrivare a quel punto, la protezione tra kernel e user si gestisce
  con il bit **U/S** delle PTE: pagine kernel = supervisor-only (U=0), pagine
  user = `USER_ACCESSIBLE` (U=1). Schema invertibile in futuro senza stravolgere
  il design.

Quando sara' fatto, diventera' necessario: un linker script a indirizzi alti,
un boot che carichi CR3 con pagine higher-half, e un VMM che condivida la meta'
alta (kernel) tra piu' PML4 per-processo mantenendo la bassa per-user.

## Riferimenti

- [Writing an OS in Rust - Heap Allocation](https://os.phil-opp.com/heap-allocation/)
- [OSDev Wiki - Bitmap Allocator](https://wiki.osdev.org/Bitmap_allocator)
- [OSDev Wiki - Paging](https://wiki.osdev.org/Paging)
- [Intel SDM - Chapter 4: Paging](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html)
