# Test Suite (Fase 9.5)

> I conteggi di suite citati negli ADR e nelle sotto-fasi del libro sono
> **snapshot all'epoca** di ciascuna fase (es. 17/17, 21/21, 32/32). Il gate
> corrente e' quello qui sotto (5/5 + 7/7 + 52/52 + shell) e in `AGENTS.md`.

La regressione automatica del sistema gira **dentro QEMU** a ogni boot: i
binari di test sono processi user reali, spawnati da `init` in sequenza prima
della shell.

## Layout

```
userland/   SOLO binari "ad uso utente": init, console, fs, devfs, shell, uptime,
            kbd, tty, disk (Fase 15/16)
libs/libr   libreria di sistema condivisa (runtime + allocatore)
testland/   test suite + repro + demo storiche
  testfs        usertestfs   — ramfs (read/write/mkdir/errori)   → PASS 5/5
  testfat       usertestfat  — FAT32 scrivibile (Fase 20) + /dev/null, /dev/zero → PASS 7/7
  usertests     usertests    — suite completa (52 test)          → PASS 52/52
  usertest-client usertestcli  — helper a modalita' (ECHO/ZEROREAD/NULLW/SRV/CHURN/KILLME/SRVDIE/SYNCWAIT/MNTDIE/OPENDIE/MAPHAMMER/FLOOD/NEST/FAULT_*/SHMDEMO/COWDEMO/FORKDEMO/ORPHAN/HARDEN/REG51/EXECDEMO)
  usertest-spin  usertestspin  — busy-loop a budget di tick (batch 512 spin puri, priorita' via SpawnMeta) + ramo SQUAT (sonda di squat FS_REGISTER, t51)
  utcbstest     utcbstest    — helper CBS: crea server e si attacha (Fase 11.5)
  hogheap / devreader         — stress/repro standalone
  demo                        — demo storica Fase 7
```

I `.bin` dei servizi/test da disco (`/bin`, `/test` su `/fat`, Fase 21) sono
iniettati a build (`scripts/inject-bins.sh`) e spawnati via `spawn_image`
(38); solo lo storage-TCB (init/disk/fs) resta embedded nel kernel via
`include_bytes!` (`kernel/src/user_binary.rs`).

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
(con i ring SPSC per-processo, Fase 10.2, nessuna race da buffer condivisi).
La shell e' spawnata per ultima.

Righe di gate:

```
[testfs] PASS 5/5
[testfat] PASS 7/7
[usertests] PASS 52/52
```

## Cosa copre `usertests` (52 test; t34 per ultimo: i drop dei diritti sono
irrevocabili sul canale della suite)

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
| t31 | presenza keyboard stack userspace (Fase 15): servizi `Kbd`/`Tty` registrati + open `/dev/kbd/kbd` e `/dev/input/keyboard` (path DEV del tty). Niente digitazione reale (serve QMP/sendkey: coperta da `test-shell.py` ~30/30) |
| t32 | disk driver in userspace (Fase 16): open raw `/dev/sda` + settore 0 con firma boot 0x55AA; kill userdisk via `service_pid` → sparizione/ricomparsa (init-restart) → raw di nuovo operativo + `/fat/HELLO.TXT` leggibile via riconnessione lazy di userfs |
| t33 | mount/umount espliciti (Fase 16b): mkdir ramfs + mount `/dev/sda`→`/mnt` + contenuto FAT + re-mount idempotente + umount busy rifiutato + umount ok (`/mnt` torna ramfs) + error paths (sorgente/target invalidi, doppio umount, umount `/`) |
| t35 | resolve nome→handle lato driver (Fase 16c): nomi ignoti senza stato (niente spec fantasma), bad-replace innocuo, mount valido operativo |
| t36 | identità stabile (Fase 16d): mount per `UUID=` e per `LABEL=` del secondo disco + contenuto MARKER (prova il disco giusto), open raw dei by-path con firma+seriale, listing sintetizzato `/dev`/by-uuid/by-label. Gira anche con `SWAP_DRIVES=1` (lettere cambiano, chiavi no) |
| t37 | snapshot processi (Fase 19.1): idle/init presenti con parent `None`, self Ready, count >= 8, TIME di init > 0 e TIME proprio crescente dopo spin |
| t38 | `stat` metadati senza open (Fase 19.2; FAT senza readonly dalla Fase 20): file/dir ramfs (size reale, vita dopo mkdir/rm), file/dir FAT, device (`/dev/null`), padri sintetizzati (`/dev`), error paths (inesistente, sotto-device) |
| t39 | servizi da disco (Fase 21): `/bin`+`/test` presenti e non vuoti, tutti i servizi registrati per nome (= boot da disco funzionante) |
| t40 | detach + reparent a init (Fase 22): MID intermedio spawna due KILLME (uno detached via flag, uno no) poi esce; foglia normale sparita da `ps`, detached viva con parent == 1, poi cleanup-kill (osservazione solo via `ps`, mai distruttiva prima del check) |
| t41 | `block_on` + echo async (ADR-0019): 1 send_async a helper MODE_SRV, raccolta con router (`on_chan`: stale scartate), teardown T_STOP+T_DONE |
| t42 | `run` 2-task + morte server (ADR-0019): due helper MODE_SRV (secondo prima per mescolare l'ordine), ogni risultato matcha il proprio req (routing per req_id, non FIFO); poi SRVDIE + kill → `ServerDied{pid,code}` esatti via router |
| t43 | composizione annidata `Join<Join<W,W>,W>` (ADR-0019): tre helper MODE_SRV, invii in ordine inverso all'albero, guidati da `block_on`; ogni risultato matcha il proprio req (routing multi-livello) |
| t44 | mmap anonimo nel basso canonico (Fase 28): pattern R/W su 3 pagine, spot-check 3 MiB multi-PT (1536 fault demand-zero), fixed/overlap/len-0/hint-disallineato rifiutati, munmap parziale rifiutato senza stato, munmap interi + riuso fixed con zeri freschi, `write` seriale da buffer mappato (prova `is_user_range` esteso) |
| t45 | protezioni (Fase 29): `mmap_prot` RO + `mprotect` RO↔RW (contenuto preservato) + →NONE (pagine cadono, riuso a zeri) + error paths (prot W-solo rifiutato, mprotect parziale rifiutato senza stato); 6 helper `utcli` che provocano fault (write su RO, read su NONE, exec su NX, write sulla guard page, `in` su porta non concessa → #GP, write a `USER_CODE` → codice RX) muoiono con `FAULT_EXIT_CODE` osservato via `EXIT_NOTIFY` |
| t46 | memoria condivisa (Fase 30): `shm_create` + `shm_map`; il parent scrive un pattern, un helper mappa la stessa regione (stesse pagine), verifica il pattern e scrive un marker che il parent vede (visibilita' bidirezionale); `munmap` rilascia il ref, id inesistente rifiutato, riuso dello slot (regione fresca a zeri) |
| t47 | shared text (Fase 32): 3 helper concorrenti dallo stesso binario condividono i segmenti immutabili (`text_stats`: `hits` cresce); alla loro morte i ref sono rilasciati (`live` cala di 3). Delta attorno alle proprie operazioni (il baseline assoluto di `live` non e' stabile: altri test lasciano reclaim pendenti) |
| t48 | COW su shm (Fase 33): il parent crea una regione (mappata normale RW) con un pattern; l'helper la mappa COW (`shm_map_cow`), legge il pattern (shared-read) e scrive due pagine (2 COW fault → copie private, `cow_count` +2); il parent non vede le scritture (isolamento); `shm_map_cow` su id inesistente rifiutato; riuso slot + zeri freschi |
| t49 | fork COW (Fase 34): l'helper duplica se stesso; padre e figlio scrivono un globale COW e verificano l'isolamento; il figlio riporta valore+return sul canale di nascita (SYNC) ed esce 0; il padre verifica report + `EXIT_NOTIFY` con code 0 |
| t50 | hardening (Fase 35, ADR-0026): un helper prova a killare un fratello (non suo figlio) e a registrare un servizio di sistema (`Init`) → entrambi rifiutati; usertests prova `map_physical` di RAM del kernel (0x100000) → rifiutato; prova a killare devfs (non suo figlio) → rifiutato (servizio vivo) |
| t51 | identita' misurata (Fase 36, ADR-0027): `peer_info` su Console/Devfs == manifest generato; stabilita' hash tra istanze; same-image positivo (X2 rimpiazza X1 vivo non-init-child, il mount sopravvive al kill); squat con hash diverso rifiutato (mount purgato, open fallisce); `peer_info` a canale morto → Err (helper REG51 + ramo SQUAT di spin) |
| t52 | exec in-place (Fase 37.0 nucleo + 37.1 argv): helper EXECDEMO diventa testspin su T_GO — stesso PID (T_ACK pre/post), hash rimisurato (diverso da prima, uguale a spin fresco), nuova immagine operativa (T_DONE); gamba argv (w1=1, exec ["ARGPROBE","hello","world"]) con report T_DONE(argc,fnv) dal fresh `_start`; reap via `poll_gone` (i `recv_done` consumano le EXIT_NOTIFY: `wait_exit` dopo sarebbe hang) |
| t34 | diritti per-canale lato server (Fase 17, per ultimo: drop irrevocabili): GET default ALL+root, drop WRITE (write -1/read ok), drop MOUNT+subtree /fat (mount/open-fuori -1, open-dentro+read+readdir-dentro ok, readdir-fuori -1), widen rifiutato + GET conferma |

> Il CBS e' sempre attivo (lo scheduler RT e' l'unico): t18/t19 sono test
> reali, non ci sono modalita' "vuote".

> I loop `recv`/`wait_reply` della suite sono **EXIT-aware** (Fase 14): le
> notifiche `EXIT_NOTIFY` che arrivano quando un helper termina vengono
> ignorate/skippate (mai scambiate per una reply o un'estranea da fallire).

### Dettagli degni di nota

- **Modalita' helper**: `usertestcli` sceglie la modalita' dal primo messaggio
  CFG dell'orchestratore. In `ZEROREAD` i client concorrenti aprono e leggono
  `/dev/zero` in parallelo (nessuna race: ogni client ha i propri ring SPSC,
  Fase 10.2); l'handshake `OPENED` + `GO` resta come semplice barriera di
  coordinamento.
- **Priorita'**: il test t17 usa High vs Normal. I server Normal idle
  (fs/shell) girano in recv-loop sempre-`Ready` (fix anti-deadlock), quindi una
  fascia `Low` non e' schedulabile finche' girano: Low resta usato solo da
  `useruptime` nel boot reale.
- **Stesso binario, piu' priorita'**: `usertestspin` gira a priorita' diverse
  via flag `prio` in `SpawnMeta` (Fase 21: t17 lo spawna a 31, gli altri usi a
  16/1). Prima della Fase 21 erano righe diverse in `NAMED_BINARIES`
  (`usertestspin`/`utspin_norm`/`utspin_high`) sullo stesso binario embedded;
  oggi solo init/disk/fs restano embedded.
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
