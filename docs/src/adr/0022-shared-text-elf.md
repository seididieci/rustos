# ADR-0022: Shared text ELF (segmenti immutabili condivisi)

## Status

Accepted (Fase 32).

## Context

La Fase 31 ha introdotto il loader ELF per-segmento: il kernel carica il
binario con i flag dell'ELF (W^X), ma **ogni processo alloca e copia l'intera
immagine** nei propri frame privati. I segmenti `R E` (codice) e `R` (rodata)
sono **immutabili**: se piu' istanze dello stesso binario girano insieme,
tenerne una copia privata a testa spreca RAM e lavoro di copia. E' il caso
d'uso canonico della memoria condivisa (Fase 30).

## Decision

Il loader divide l'immagine al confine `rw_off` = `align_down(min p_vaddr` dei
segmenti con `PF_W`)`, o `end` se non ci sono segmenti scrivibili:

- `[base, rw_off)`: immutabile (codice `RX` + rodata `RO`). Condiviso tra le
  istanze dello stesso binario tramite `kernel/src/text.rs`: tabella statica di
  16 immagini, ognuna con `phys/pages/hash/base/rw_off/refs`. Le pagine sono
  mappate **read-only e non-owned** (`map_user_leaf_shared`).
- `[rw_off, end)`: privato (data/bss + la coda immutabile della pagina a
  cavallo), allocato e mappato `owned` come prima.

Identita' dell'immagine: hash FNV-1a dell'ELF **piu' verifica byte-per-byte**
del contenuto immutabile su hit. L'input da disco (`spawn_image`) non e'
fidato: una collisione di hash mapperebbe codice sbagliato, quindi il solo
hash non basta. Slot pieni o nessuna parte condivisibile → fallback al load
privato (mai spawn fallito).

Il riferimento e' tracciato nel PCB (`text_id`, 0 = nessuno) e rilasciato in
`reclaim_one` **dopo** il teardown dell'address space: il walk di teardown
libera solo le foglie `owned`, quindi le pagine condivise vengono staccate ma
non liberate; `text::release` le libera quando `refs` arriva a 0. Scope:
**refcount-only** (condivide tra istanze concorrenti, libera a 0); una cache
persistente con eviction e' un follow-up.

## Consequences

### Positive

- Le istanze concorrenti dello stesso binario condividono le pagine
  immutabili: meno RAM, meno copia al secondo spawn in poi.
- `shm` (Fase 30) trova un uso reale nel percorso di spawn; il refcount dei
  frame resta kernel (ownership), coerente con ADR-0005.
- Base per "librerie condivise" e binari grandi futuri.

### Negative

- Superficie in piu' nel loader (hash + verifica) e un campo nel PCB.
- Con lo scope refcount-only **non** c'e' guadagno tra spawn sequenziali (il
  testo e' liberato a 0): serve una cache persistente.
- Il valore e' **memoria/architetturale**, non throughput: lo spawn e'
  dominato dalla lettura FS/disco (cache di settore, Fase 25), non dalla copia.

### Neutral

- `text_stats` (syscall 44) espone `hits/misses/live` per il test.
- La pagina a cavallo `rw_off` e' copiata nel blocco privato (RO+W): il
  condiviso si ferma a `rw_off`.

## Alternatives Considered

- **Cache persistente delle text image**: eviterebbe anche il rebuild tra
  spawn sequenziali, ma richiede eviction/politica di memoria e un contratto
  di pressione oggi inesistente (`sbrk` cresce soltanto) — rimandata.
- **COW dell'intera immagine**: darebbe anche il non-copy del `.data`, ma e'
  fuori scope (fault handler + copy-on-write).
- **Condivisione a livello di shm generico**: shm mappa NX e senza flag
  per-pagina; il testo richiede `RX`/`RO` misti → modulo dedicato.

## References

- `kernel/src/text.rs`, `kernel/src/elf.rs`, `kernel/src/vmm_user/paging.rs`
  (`map_user_leaf_shared`), `kernel/src/process.rs` (`text_id`),
  `kernel/src/sched_rt/lifecycle.rs` (`reclaim_one`)
- `docs/src/04-memory.md` (sezione "Shared text")
- ADR-0021 (loader ELF), Fase 30 (memoria condivisa)
