# ADR-0018: Cache settoriale write-through in userdisk

**Status**: Implemented (Fase 25)
**Data**: 2026-09-17

## Contesto

Dopo 24 il collo di bottiglia resta il percorso disco: un settore costa
~1.2 ms (2 round-trip IPC + handoff scheduler + ~266 uscite KVM di polling
PIO), e ogni op logica FAT = MANY settori (walk catena con re-read del
settore FAT, find per open, read-modify-write + FLUSH per settore). Il bench
`fat_small_orc` (open+read+close di 25 B) pagava ~6.1M cicli/op sullo stesso
host della baseline — quasi tutto PIO ripetuto sugli stessi settori (BPB,
tabella FAT, directory root).

Due collocazioni possibili:

- (a) **cache file/range in `userfs`** (percorso per-file);
- (b) **cache settore in `userdisk`** (percorso per-blocco).

## Decisione

**Un solo strato di cache, a livello blocco/settore in `userdisk`**
(`userland/disk/src/cache.rs`), write-through, 256 entry fisse (~128 KiB in
`.bss`). Rimosso il memo da 1 settore `fat_memo` in `userfs` (`fat32.rs`,
24.1): la cache del driver lo subsume.

Motivi:

1. **Indipendenza dal consumatore**: lo stesso strato serve dati e metadati
   FAT, i nodi raw `/dev/sdX` (relay `DEV_*`) e qualunque FS futuro (ext2)
   senza toccarli. Una cache file dovrebbe comunque invalidarsi sui metadati
   e non coprirebbe i raw.
2. **Niente doppia cache**: due strati (file + blocchi) terrebbero gli stessi
   512 byte due volte in RAM. Un solo strato = niente RAM sprecata, niente
   protocollo di coerenza tra strati.
3. **Zero cambi di protocollo**: `DISK_*` (tag in `syscall-numbers`, frame
   multi 24.2) invariato — la cache e' un dettaglio interno di
   `node_read(_multi)` / `node_write(_multi)`. `userfs`, `libr`, il kernel
   non cambiano.
4. **Coerenza gratis**: chiave fisica `(disco, lba_fisica)` (base nodo + lba
   relativa), quindi whole-disk e partizioni condividono la entry e
   `DISK_*` + `DEV_*` non possono divergere.
5. **Stessa durabilita' di Fase 20**: write-through = prima il PIO + FLUSH
   stabile, poi l'update della cache; a errore IO la entry e' invalidata
   (mai servire cio' che non e' atterrato).

Dettagli implementativi:

- Array statico, **zero allocazioni heap nel per-op** (regola 24.2: i `Vec`
  temporanei frammentavano la free-list e degradavano tutte le op).
  `userdisk` e' single-threaded: `static mut` con accessi via raw pointer
  (`addr_of_mut!`, richiesto dall'edition 2024), nessun lock.
- Eviction **CLOCK** (second chance) su 256 entry; lookup lineare (sub-µs
  contro ~1.2 ms di PIO).
- I miss contigui restano **un solo comando PIO** (24.2 preservato): il run
  di miss e' delimitato con `contains()` e letto in una `read_sectors`.
- Contatori `hits/misses/inserts` con log throttled ogni 2048 accessi
  (`[userdisk] cache hits=…`): servono a dimensionare la futura dinamica.
- Hook per il futuro senza biforcazioni oggi: `enum Policy`
  (`WriteThrough` attivo, `dirty` sempre false) in un punto solo
  (`CACHE_SECTORS`, `POLICY`).

## Misure (KVM, stesso host, media 3 run, TSC ~1.6 GHz)

| Op | 24 (no cache) | 25 (cache) | Effetto |
|----|---------------|------------|---------|
| `zero_1B` | 5.9K cyc | 6.9K cyc | invariato (no disco) |
| `sda_512B_seq` | 3.04M cyc | 3.75M cyc | invariato entro il rumore¹ |
| `fat_small_orc` | 6.14M cyc (~6 KiB/s) | 49K cyc (~838 KiB/s) | **~126x** |
| `ramfs_4K_write/read` | 93K/64K cyc | 117K/76K cyc | invariato entro il rumore¹ |
| `fat_4K_oow` | 86M cyc (~81 KiB/s) | 46M cyc (~146 KiB/s) | **~1.9x** |

¹ Su questo host il rumore KVM/DVFS tra run identici arriva a ±20–40% sulle
op brevi (tsc 1.62–1.68 GHz tra run; `fat_4K_oow` baseline 57–97 KiB/s sullo
stesso binario). Si dichiara solo cio' che supera di molto il rumore.

Hit rate osservati: bench 74% (1514 hit / 534 PIO su tutto il bench),
suite di regressione 91%.

Gate invariato: `[testfs] PASS 5/5` + `[testfat] PASS 7/7` + `[usertests]
PASS 40/40` + shell 30/30, zero FAIL/PANIC/FAULT.

## Sviluppi futuri (non in 25)

- **Write-back configurabile**: accendere `Policy::WriteBack` usando il campo
  `dirty` + flush epoch (per-mount o per-timer). Richiede ragionamento su
  crash-consistency (ordine FAT→dati→dir entry di Fase 20.3) e un punto di
  flush esplicito (sync/umount/kill). Guadagno atteso: `fat_4K_oow` senza PIO
  sincrono per write.
- **Cache dinamica con reclaim**: crescere via `sbrk` in base alla RAM e
  cedere pagine sotto pressione. Bloccata da un contratto oggi inesistente —
  `sbrk` cresce soltanto, nessun canale kernel→driver per chiedere memoria.
  I contatori 25 servono a dimensionarla quando il contratto esistera'.
- **Read-ahead sequenziale**: `sda_512B_seq` resta freddo per costruzione
  (ogni settore letto una volta). Un prefetch di N settori dopo K miss
  sequenziali lo coprirebbe; oggi volutamente assente (ogni PIO in piu' e'
  un rischio di sprecarlo).
- **Niente secondo strato in `userfs`**: se mai servira' un memo client,
  dovra' essere esclusivo (solo cio' che il driver non cacherebbe), mai una
  seconda copia degli stessi settori.
- **`DISK_STATS` esplicita**: oggi i contatori viaggiano solo sul seriale.
  Se un giorno serviranno a un tool (`stat` della cache da shell), un tag
  dedicato — non in 25 per non allargare il protocollo.

## Conseguenze

- `userdisk.bin` cresce di ~128 KiB di `.bss` (statici, non di codice).
- `fat32.rs` perde `fat_memo` + import `Cell`: un solo strato di cache in
  tutto il sistema, come da decisione.
- La regola "mai heap nel per-op dei server" si estende alla cache: array
  fisso, bound statici, log throttled.
