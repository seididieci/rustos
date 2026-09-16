# ADR-0017: Servizi caricati da disco (via `spawn_image`)

Data: 2026-09-16
Stato: accettato (Fase 21)

## Contesto

Il kernel embeddava TUTTI i binari user (`NAMED_BINARIES`, ~15 binari via
`include_bytes!`): ogni servizio/test ingrossava il kernel (lezione Fase 17:
il `.bss` ha superato la boot map da 2 MiB → triple fault) e nessun servizio
era aggiornabile senza rebuild del kernel. Lo `spawn` per nome legava i
processi alla tabella statica compilata nel kernel.

## Decisione

Primitiva generale di creazione + storage-TCB minimo:

- **`SYS_SPAWN_IMAGE` (38)**: come `spawn` ma il binario e' letto dalla
  memoria del chiamante (come fork+exec). `SpawnMeta` 40 B `repr(C)`
  (identico in `libr`): nome NUL-padded 16 B (non vuoto, stampabile), prio
  1..31 (mai 0/idle), fino a 4 range I/O. Il kernel valida tutto (fail-loud,
  mai UB), copia in frame privati con coda azzerata (igiene .bss), bound
  256 KiB per singolo spawn. Le **porte I/O sono privilegio root**: solo pid 1
  (init) puo' chiederle, gli altri con `io_count == 0`. Nome display owned nel
  PCB (`name_owned`, max 16 B), birth channel condiviso con `spawn`.
- **Storage-TCB embedded**: solo init/disk/fs (caricati prima che il FS
  esista). Tutto il resto vive in `/bin` (servizi) e `/test` (suite) su /fat,
  iniettati a build via `scripts/inject-bins.sh` (nomi 8.3 senza prefisso
  `user`, single source di run.sh e test-shell.py).
- **Manifest in init**: tabella path/prio/porte; boot disk → fs → console (da
  disco: richiede Fs pronto) → uptime/devfs → kbd → tty → test in sequenza →
  shell. **Restart rileggono sempre da disco** (niente cache binari:
  freschezza garantita), fail-loud a boot.
- Suite: helper via `spawn_image` da `/fat/test`, nuovo t39 (`/bin`+`/test`
  presenti e servizi up). Suite → 39/39.

## Conseguenze

- Un load da 30 KB costa ~480 round-trip DISK (OPEN per settore + find per
  read + chunk da 2 KB) e sotto carico ogni handoff attende i quanti degli
  spinner a pari prio (t24: 15 → 3560 tick, restart 10 s+timeout). Fix senza
  cambiare semantica: helper sacrificali in recv-block, `spin_ticks` batch
  512, chunk load 4000 B (= RING_MAX_PAYLOAD), cache FileInfo per-fd con
  generazione (bump a ogni mutazione FAT; stat sempre fresca, niente fd),
  `IpcDisk` OPEN-once per connessione, bound t27 Fase B/C a 2000 tick.
  Lezione: MAI spinner a pari prio dei server; i costi si misurano in
  round-trip, non in tick (non confrontabili tra TCG/KVM).
- Bug trovato: `wait_ready` ingoiava le EXIT_NOTIFY altrui → restart persi
  (userdisk morto durante il restart di devfs) → stash + drain nei loop.
- Validazione: gate 5/5 + 7/7 + 39/39 (×2 TCG + ×2 reorder), shell 30/30 KVM,
  reorder UUID PASS, zero FAIL/PANIC.

## Alternative scartate

- **Cache binari in RAM (init/usertests)**: restart piu' veloci ma binari
  potenzialmente stale; scelto reload-always (il restart deve vedere il disco
  nuovo). Rivalutare solo con versionamento esplicito.
- **`spawn` per-nome esteso**: la tabella statica non scala e ricompila il
  kernel a ogni servizio nuovo. Rimane solo per lo storage-TCB.
- **Anche fs/disk da disco**: impossibile senza chicken-and-egg (il FS serve
  per leggere il disco). TCB minimo confermato.
- **FAT cache / read-ahead**: ridurrebbe ancora i round-trip ma aggiunge
  stato e rischio stale; rimandata (i bound attuali hanno margine).
