# Performance (Fase P0 — baseline)

> Piattaforma di riferimento: **KVM** (`-accel kvm -cpu host`). I tempi TCG
> sono emulati e NON di riferimento. Orologio: TSC in ring 3 (CR4.TSD mai
> impostato), calibrato sul PIT via `libr::tsc_calibrate` (~4.45 GHz sul
> riferimento). Nessuna cache nel percorso dati: ogni op attraversa IPC +
> userfs + userdisk + PIO, quindi i numeri misurano il percorso vero.

## Harness

- `testland/bench` (`userbench`, `/test/bench.bin`): 6 op end-to-end con
  warmup, stampa righe `[bench] <nome> iters=<n> cyc_op=<c> kb_s=<k>`.
- `scripts/bench.sh` (`RUNS=3`, `TIMEOUT_S=120`): N boot KVM, fail-loud se il
  bench non completa (`DONE ok=1`). Esecuzione: `RUN_BENCH=1` compila init
  con feature `bench` (ortogonale a `skip_tests`): il bench gira dopo
  l'eventuale suite, prima della shell — **mai nel gate** di regressione.

## Baseline (KVM, media 3 run, TSC ~4.45 GHz)

| Op | Cosa attraversa | cyc/op | Latenza | Throughput |
|----|-----------------|--------|---------|------------|
| `zero_1B` (2000× read 1 B `/dev/zero`) | solo IPC + ring | ~8.2 K | ~1.9 µs | ~526 KiB/s |
| `sda_512B_seq` (200× read 512 B `/dev/sda`) | IPC + userdisk + PIO 1 settore | ~5.43 M | ~1.2 ms | ~409 KiB/s |
| `fat_small_orc` (500× open+read+close 25 B) | find + IPC + PIO | ~92.6 M | ~20.8 ms | ~1 KiB/s |
| `ramfs_4K_write` (100× write 4 KiB) | FS + IPC, niente disco | ~110 K | ~25 µs | ~161 MiB/s |
| `ramfs_4K_read` (100× read 4 KiB) | FS + IPC, niente disco | ~62 K | ~14 µs | ~289 MiB/s |
| `fat_4K_oow` (50× open+overwrite+close 4 KiB) | IPC + PIO + FLUSH/settore | ~285 M | ~64 ms | ~62 KiB/s |

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
  il riferimento per le ottimizzazioni (P1) è il throughput, non la
  latenza minima.

## Soglia di non-regressione

Peggioramento > 10% su una qualunque riga (stesso host KVM, media 3 run)
= fail. Rivalutare la baseline solo a parità di hardware e versione QEMU.
