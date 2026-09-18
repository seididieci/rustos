# Performance (Fasi 23–25 — baseline + ottimizzazioni)

> Piattaforma di riferimento: **KVM** (`-accel kvm -cpu host`). I tempi TCG
> sono emulati e NON di riferimento. Orologio: TSC in ring 3 (CR4.TSD mai
> impostato), calibrato sul PIT via `libr::tsc_calibrate` (~4.45 GHz sul
> riferimento 23/24, ~1.6 GHz sull'host della campagna 25: confrontare solo
> misure dello STESSO host). Nessuna cache nel percorso dati fino a 24
> (25 aggiunge la cache settoriale, vedi sotto).

## Harness

- `testland/bench` (`userbench`, `/test/bench.bin`): 6 op end-to-end con
  warmup, stampa righe `[bench] <nome> iters=<n> cyc_op=<c> max_cyc=<m> kb_s=<k>`.
- `scripts/bench.sh` (`RUNS=3`, `TIMEOUT_S=300`): N boot KVM, fail-loud se il
  bench non completa (`DONE ok=1`). Esecuzione: `RUN_BENCH=1` compila init
  con feature `bench` (ortogonale a `skip_tests`): il bench gira dopo
  l'eventuale suite, prima della shell — **mai nel gate** di regressione.

## Baseline 23 (KVM, media 3 run, TSC ~4.45 GHz) → 24 (stessa base)

| Op | 23 cyc/op | 24 cyc/op | 24 latenza | 24 throughput |
|----|-----------|-----------|------------|---------------|
| `zero_1B` (2000× read 1 B `/dev/zero`) | ~8.2 K | ~8.3 K (invariato) | ~1.9 µs | — |
| `sda_512B_seq` (200× read 512 B `/dev/sda`) | ~5.43 M | ~5.45 M (invariato) | ~1.2 ms | ~409 KiB/s |
| `fat_small_orc` (500× open+read+close 25 B) | ~92.6 M | ~10.9 M (**8.5x**) | ~2.4 ms | ~10 KiB/s |
| `ramfs_4K_write` (100× write 4 KiB) | ~110 K | ~109 K (invariato) | ~25 µs | ~161 MiB/s |
| `ramfs_4K_read` (100× read 4 KiB) | ~62 K | ~61 K (invariato) | ~14 µs | ~289 MiB/s |
| `fat_4K_oow` (50× open+overwrite+close 4 KiB) | ~285 M | ~129 M (**2.2x**) | ~30 ms | ~135 KiB/s |

## 24 — cosa ha funzionato

- **24.1 (solo userfs, nessun cambio di protocollo)**: memo dell'ultimo
  settore FAT (invalidata a `set_fat_entry`, azzerata a ogni epoca) +
  letture a settori mirati (`read_file` per span, `read_dir` con stop al
  terminatore 0x00 invece dell'intero cluster). Da sola: small FAT 8.5x.
- **24.2 (protocollo DISK v2, stessi tag)**: frame con count (≤7/IPC, bound
  del ring), 1 comando PIO per run (`AtaDisk::read/write_sectors`), 1
  FLUSH CACHE per write (prima: comando+flush a settore). Da sola sopra
  24.1: overwrite 4K 36 ms → 30 ms. `BlockSource::{read,write}_sectors`
  (default a loop, `IpcDisk` in vero multi); DEV relay intatto.

## Lezione 24.2 — heap dei server: niente `Vec` temporanei per-op

A metà 24.2 i bench crollavano progressivamente (write ramfs 25 µs →
2 ms) in proporzione alle op FAT precedenti — con gate verde e risultati
corretti. Diagnosi (strumento temporaneo `libr::heap::heap_stats`,
mantenuto): la free-list first-fit di userfs cresceva di ~1 blocco a op
FAT (temp `Vec` 4K/512 B liberati tra blocchi vivi, mai coalescibili) e
ogni allocazione paga O(n) + O(n²) di `coalesce()` su tutte le op
successive, anche ramfs. Cura: hot path FAT zero-alloc — parse dir
incrementale con offset aritmetici, run in buffer stack a chunk ≤8,
risposta read in stack (count ≤ 4096 già garantito). Regola: **nei server,
mai allocazioni heap nel percorso per-op** (solo a setup/mount).

## Lettura

- Lo stack FS+IPC senza disco vola (14–25 µs): **il collo di bottiglia è
  il percorso disco**, non l'IPC in sé (floor ~2 µs/op).
- Un settore da disco costa ~1.2 ms: 2 round-trip IPC + handoff scheduler +
  ~266 uscite KVM (polling PIO porta per porta).
- `fat_small_orc` (~21 ms per 25 B) e `fat_4K_oow` (~64 ms) mostrano il
  moltiplicatore: ogni op logica = MANY settori (walk catena FAT con re-read
  del settore FAT a ogni cluster, find per open, read-modify-write +
  FLUSH CACHE dedicato per settore in scrittura).
- Le latenze a singola op interagiscono col quanto scheduler (20 ms):
  il riferimento per le ottimizzazioni (24) è il throughput, non la
  latenza minima.

## Soglia di non-regressione

Peggioramento > 10% su una qualunque riga (stesso host KVM, media 3 run)
= fail. Rivalutare la baseline solo a parità di hardware e versione QEMU.

## 25 — cache settoriale write-through in userdisk (ADR-0018)

Un solo strato di cache a blocchi nel driver (`userland/disk/src/cache.rs`:
256 entry, chiave fisica `(disco, lba)`, CLOCK, write-through, zero heap nel
per-op); `fat_memo` (24.1) rimosso da `userfs`. Protocollo `DISK_*` invariato.

Confronto A/B **sullo stesso host** (KVM, media 3 run, TSC ~1.6 GHz;
la tabella 24 sopra e' di un altro host e NON e' confrontabile):

| Op | 24 stesso host (cyc/op) | 25 (cyc/op) | Effetto |
|----|-------------------------|-------------|---------|
| `zero_1B` | ~5.9 K | ~6.9 K | invariato (no disco) |
| `sda_512B_seq` | ~3.04 M | ~3.75 M | invariato entro il rumore¹ |
| `fat_small_orc` | ~6.14 M (~6 KiB/s) | ~49 K (~838 KiB/s) | **~126x** |
| `ramfs_4K_write` | ~93 K | ~117 K | invariato entro il rumore¹ |
| `ramfs_4K_read` | ~64 K | ~76 K | invariato entro il rumore¹ |
| `fat_4K_oow` | ~86 M (~81 KiB/s) | ~46 M (~146 KiB/s) | **~1.9x** |

¹ Rumore misurato: su questo host run identici dello stesso binario variano
±20–40% sulle op brevi (DVFS: tsc 1.62–1.68 GHz tra run; `fat_4K_oow`
baseline 57–97 KiB/s). Si dichiara solo cio' che supera di molto il rumore:
le re-read degli stessi settori (small FAT) non pagano piu' PIO; gli
overwrite pagano ancora PIO+FLUSH per settore (write-through) ma le re-read
di FAT/dir vanno in cache; il sequenziale freddo resta PIO (niente
read-ahead in 25, volutamente).

Hit rate: bench 74% (1514 hit / 534 settori via PIO), suite 91%.
Gate invariato (5/5 + 7/7 + 44/44 + shell 30/30).
