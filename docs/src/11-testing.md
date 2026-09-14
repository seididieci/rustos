# Test Suite (Fase 9.5)

La regressione automatica del sistema gira **dentro QEMU** a ogni boot: i
binari di test sono processi user reali, spawnati da `init` in sequenza prima
della shell.

## Layout

```
userland/   SOLO binari "ad uso utente": init, console, fs, devfs, shell, uptime
libs/libr   libreria di sistema condivisa (runtime + allocatore)
testland/   test suite + repro + demo storiche
  testfs        usertestfs   — ramfs (read/write/mkdir/errori)   → PASS 5/5
  testfat       usertestfat  — FAT32 read-only + /dev/null, /dev/zero → PASS 6/6
  usertests     usertests    — suite completa (33 test)          → PASS 33/33
  usertest-client usertestcli  — helper a modalita' (ECHO/ZEROREAD/NULLW/SRV)
  usertest-spin  usertestspin  — busy-loop a budget di tick (priorita')
  utcbstest     utcbstest    — helper CBS: crea server e si attacha (Fase 11.5)
  hogheap / devreader         — stress/repro standalone
  demo / srv / cli            — demo storiche Fase 7
```

I `.bin` vengono inclusi nel kernel via `include_bytes!`
(`kernel/src/user_binary.rs`) e sono spawabili per nome.

## Esecuzione

```bash
./run-tests.sh           # gate di regressione: boot CON la suite + QEMU
./run.sh                 # produzione: boot SENZA test, shell subito usabile
```

Di default (`./run.sh`) init SALTA i test (feature `skip_tests`, shell subito
usabile in ~2 s); con `RUN_TESTS=1` (`./run-tests.sh`) init spawa i test in
SEQUENZA, aspettando un IPC `TEST_DONE` (tag `0x7E`)
da ciascuno prima dello spawn successivo: i tre binari condividono la ramfs di
userfs (path e file di lavoro) e la sequenza rende output e PID deterministici
(dal buffer FS per-processo, Fase 9.6, la vecchia race sulla shared buffer
page non esiste piu').
La shell e' spawnata per ultima.

Righe di gate:

```
[testfs] PASS 5/5
[testfat] PASS 6/6
[usertests] PASS 33/33
```

## Cosa copre `usertests` (33 test)

| Test | Cosa verifica |
|------|----------------|
| t1 | getpid |
| t2 | ticks monotoni (timer attivo, IF=1 in user) |
| t3 | heap lazy demand-zero: pagina sbrk fresca letta = 0 |
| t4 | allocatore reuse/coalescenza |
| t5 | spawn + getpid del figlio |
| t6 | ramfs: read hello.txt |
| t7 | ramfs: write multi-chunk (>1 pagina FS) + read-back |
| t8 | ramfs: mkdir + readdir |
| t9 | FS: error paths (open path vuoto, fd invalido) |
| t10 | /dev/null |
| t11 | /dev/zero |
| t12 | map_physical aliasing (pagina scratch kernel `MAP_TEST_PHYS`) |
| t13 | IPC single echo |
| t14 | IPC multi-client (reply_target, no cross-talk) |
| t15 | devfs concorrente + heap churn (regressione lazy/IPC) |
| t16 | preemption ring-3 (contatore su pagina scratch) |
| t17 | priorita' High > Normal |
| t18 | CBS admission control |
| t19 | CBS bandwidth: audio (CBS 30%) + hog (no CBS) |
| t20 | FS async 1-in-volo (Fase 13) |
| t21 | IPC async N-in-volo + backpressure (Fase 13) |
| t22 | lifecycle churn (Fase 14): 42 spawn/exit di helper CHURN (~2 MiB heap ciascuno) oltre il vecchio limite cumulativo → riuso PID + niente frame leak (notifiche `EXIT_NOTIFY` attese per ogni figlio) |
| t23 | kill + exit notify (Fase 14): kill di un helper KILLME con code noto → notifica con (code, pid); il pool accetta ancora spawn |
| t24 | notifica unificata di morte (Fase 14): server SRVDIE (registra `Service::Test`, mai risponde) + client SYNCWAIT (lookup + send sync bloccato); kill → `wait_reply` da' `ServerDied{pid,code}` esatti (path async), il client sbloccato osserva EXIT_NOTIFY e riporta T_DONE (path sync), slot servizio liberato, pool sano |
| t25 | purge mount alla morte driver (Fase 14): driver MNTDIE registra `/tdie`, open instradato, kill, re-registrazione stesso prefix → open via nuovo driver (senza purge lo stale avvelenerebbe `resolve_mount`); kill D2 + smoke ramfs |
| t26 | purge rings/ftable alla morte client (Fase 14): 10 helper OPENDIE aprono /dev/null+/dev/zero+hello.txt e muoiono senza close → smoke FS completo (null/zero/hello/write/mkdir/readdir) prova server sano |
| t27 | init-restart di devfs (Fase 14): kill via `service_pid` → sparizione dallo slot → ricomparsa (pid anche riusato: osserva sparizione→ricomparsa, non confronto) → /dev/null di nuovo operativo + smoke ramfs |
| t28 | restart di userfs end-to-end (Fase 14): kill via `service_pid` → fixture fresh (mkdir/write/read), hello.txt ricreato, probe ramfs sparito (wipe via readdir), /fat leggibile (persistente), /dev/null operativo (driver re-registrati) |
| t29 | map-flap isolation (diagnosi t28): martella `map_physical` su una VA verificando marker, da solo poi con helper sulla stessa VA (altre tabelle/frame) → niente cross-talk |
| t30 | fairness scheduler sotto carico IPC: helper FLOOD (open+write+close /dev/null a regime dopo warm-up) + kill devfs + latenza mount (bound 300 tick, osservato 0–1) → becca regressioni di rotazione/starvation (es. bug di parita' round-robin). NON misura saturazione userfs: con client sync (≤1 in volo) la coda non si riempie mai |
| t31 | presenza keyboard stack userspace (Fase 15): servizi `Kbd`/`Tty` registrati + open `/dev/kbd/kbd` e `/dev/input/keyboard` (path DEV del tty). Niente digitazione reale (serve QMP/sendkey: coperta da `test-shell.py` 3/3) |
| t32 | disk driver in userspace (Fase 16): open raw `/dev/sda` + settore 0 con firma boot 0x55AA; kill userdisk via `service_pid` → sparizione/ricomparsa (init-restart) → raw di nuovo operativo + `/fat/HELLO.TXT` leggibile via riconnessione lazy di userfs |
| t33 | mount/umount espliciti (Fase 16b): mkdir ramfs + mount `/dev/sda`→`/mnt` + contenuto FAT + re-mount idempotente + umount busy rifiutato + umount ok (`/mnt` torna ramfs) + error paths (sorgente/target invalidi, doppio umount, umount `/`) |

> Il CBS e' sempre attivo (lo scheduler RT e' l'unico): t18/t19 sono test
> reali, non ci sono modalita' "vuote".

> I loop `recv`/`wait_reply` della suite sono **EXIT-aware** (Fase 14): le
> notifiche `EXIT_NOTIFY` che arrivano quando un helper termina vengono
> ignorate/skippate (mai scambiate per una reply o un'estranea da fallire).

### Dettagli degni di nota

- **Modalita' helper**: `usertestcli` sceglie la modalita' dal primo messaggio
  CFG dell'orchestratore. In `ZEROREAD` i client concorrenti aprono e leggono
  `/dev/zero` in parallelo (nessuna race: ogni client ha la propria pagina FS,
  Fase 9.6); l'handshake `OPENED` + `GO` resta come semplice barriera di
  coordinamento.
- **Priorita'**: il test t17 usa High vs Normal. I server Normal idle
  (fs/shell) girano in recv-loop sempre-`Ready` (fix anti-deadlock), quindi una
  fascia `Low` non e' schedulabile finche' girano: Low resta usato solo da
  `useruptime` nel boot reale.
- **Stessa bin, piu' priorita'**: `usertestspin` e' esposto a piu' priorita'
  tramite piu' righe in `NAMED_BINARIES` (`usertestspin` Low, `utspin_norm`,
  `utspin_high`) che condividono phys/frames dello stesso binario.
- **Polling throttled nei test di restart (t27/t28)**: le attese di
  operativita' riprovano ogni ~20 tick via `libr::poll_wait`/`open_wait`,
  MAI in busy-loop su syscall FS. Igiene da buon vicinato (Livello 1):
  ogni tentativo e' un round-trip servito da userfs e non c'e' motivo di
  inondarlo. NOTA di onesta': l'attribuzione causale del vecchio FAIL t27
  al solo storm e' debole (N=1; la coda da 8 slot con client sync non puo'
  saturarsi per costruzione) — il throttle resta come disciplina, non come
  fix provato. Esperimento B (Fase C in busy-loop non throttled): t27 PASSA
  comunque → self-storm NON causale, confound confermato. Vedi t30 per il
  gate di fairness.
- **Binario copiato per processo** (`user_binary.rs::copy_binary`): i frame del
  binario embedded vengono copiati in frame privati a ogni spawn. Mappare gli
  stessi frame a piu' processi condividerebbe `.bss`/`.data` mutabili (es. la
  free-list dell'allocatore di `libr`) e corromperebbe lo stato di due istanze
  della stessa bin.
