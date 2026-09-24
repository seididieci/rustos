# ADR-0011: Tastiera e terminale in userspace (userkbd + usertty)

**Status**: Implemented (Fase 15)
**Data**: 2026-09-11

## Contesto

Fino alla Fase 14 la tastiera era l'ultimo driver nel kernel: `kbd_process`
(ring 0, priorita' High) leggeva la porta PS/2 `0x60`, accodava scancode e li
iniettava al console server via IPC; il console faceva decode (`pc_keyboard`),
echo su VGA e serviva `/dev/input/keyboard`. Il kernel possedeva quindi tre
file (`kbd_process.rs`, `kbd_events.rs`, `keyboard.rs` con init i8042) + uno
spawn dedicato, contro il disegno microkernel (ADR-0005: solo scheduling, IPC,
memoria e routing interrupt nel kernel).

## Decisione

Split in tre processi userspace, tutti a priorita' Normal:

- **`userkbd`** (driver PS/2 puro): possiede le porte `0x60/0x64` via
  `io_ranges` (TSS per-processo, ADR-0006), esegue init i8042 in userland,
  pubblica scancode raw (Set 1) su `/dev/kbd`, servizio `Kbd`. Bloccato in
  `recv`, svegliato da IRQ1.
- **`usertty`** (terminal server): decode `pc_keyboard` (layout US), echo su
  `/dev/console`, byte cotti su `/dev/input/keyboard` con protocollo
  byte-identico a prima (la shell NON cambia). Servizio `Tty` (solo per la
  supervisione init; i client usano il FS).
- **Fase 43b (emendamento)**: tty e' raw — decodifica frecce/Home/End/Delete
  in sequenze ESC e NON fa piu' echo (lo fa il lettore: la readline della
  shell, echo console-only). Il floor backspace 18.0 migra nella shell.
- **`userconsole`** (solo rendering): VGA + cursore, device di output
  `/dev/console` (`DEV_CONSOLE=3`, nuovo `dev_type` in userfs).

Il kernel su IRQ1 fa solo routing + EOI: risolve `Kbd` per nome e sveglia
l'owner (`wake`, con `pending_wake` anti-lost-wakeup esistente). Non legge piu'
porte, non ha code, non ha processi oltre `idle` + `init`.

Init spawna console→fs→uptime→devfs→kbd→tty (+shell/test) e supervisiona
kbd/tty come gli altri servizi (restart/backoff Fase 14).

## Regole emerse (vincolanti per i futuri driver-server)

1. **Mai IPC sincrone mentre si serve.** userfs inoltra relay DEV e resta
   bloccato finche' il driver non risponde; se il driver resta bloccato su
   userfs (pump read, echo write) → ciclo userfs↔driver, wedge totale
   (osservato t30/t31). I driver che sono anche client FS usano SOLO il
   percorso async nonbloccante (`fs_op_async`/`read_async`/`write_async`/
   `open_async`/`fs_register_async`/`fs_buf_reg_async` + collect via poll),
   rispondono alle relay all'istante (i DEV_WRITE si accodano: le scritte VGA
   non falliscono) e dormono in `recv()` (event-driven).
2. **Mai spin a pari priorita'.** Uno spinner Normal ruba ~1 quantum per hop
   IPC agli altri (misurato: flooder t30 da 3000+ op a 64 op/10s con UN solo
   spinner; il parent in `recv_poll` faceva lo stesso). Test e server
   attendono BLOCCANDOSI; il polling e' throttled o event-driven.
3. **Boot async senza attese di wake.** Prima della registrazione nessuno puo'
   svegliare il driver: l'invio avviene NELLA STESSA chiamata che entra nella
   fase (mai "fase ora, invio al prossimo giro"), altrimenti un `recv()`
   bloccante pre-invio dorme per sempre. Stessa regola per il dormire: si
   dorme solo con wake garantito (pending settata, Steady con lavoro reale),
   altrimenti si polla come gli ensure loop.
4. **Handshake per canale.** La tabella `rings` di userfs e' indicizzata per
   canale: sotto un nuovo canale (restart userfs, re-lookup) serve un nuovo
   `FS_BUF_REG` o ogni op prende `NOHANDSHAKE` per sempre.
5. **Aprire file device, mai mount-root.** Aprire `/dev/kbd` (radice) da'
   `rel=""` → `dev_type` fallisce (EISDIR): si apre `/dev/kbd/kbd`.
6. **Hardening PS/2 in GTK (mouse vivo).** Init i8042 con wait IBF/OBF bound
   e `drain_hw` che legge ma scarta i byte AUX (mouse) e con errori di
   parita'/timeout, mai accodati: senza, lo stream si corrompe con caratteri
   sbagliati/ripetuti, visibili solo in GTK col mouse vivo (non con `sendkey`
   QMP).

## Conseguenze

- `libr` espone l'API FS async generalizzata (`fs_op_async` con tag IPC
  parametrico — `FS_NOTIFY` per le op, `FS_REGISTER` per la registrazione —,
  `write_async`/`open_async`/`fs_register_async`/`fs_buf_reg_async`,
  `fs_collect_msg` nonbloccante, `fs_abort_pending`).
- `channels::alloc` riusa canali esistenti (`find`): `service_lookup`
  ripetuti non bruciano piu' il pool da 128 (panic osservata). `spawn` non
  fa piu' panic a pool esaurito (ritorna -1).
- Nuovo tag `KBD_NOTIFY` (0x40, fire-and-forget senza reply) da kbd a tty.
- Nuovo tag `IRQ_NOTIFY_KBD` (0x41, kernel→userkbd): il bridge interrupt→IPC.
  L'handler IRQ1 del kernel risolve l'owner `Kbd` per nome e gli accoda una
  notify; senza un messaggio in coda, un driver che dorme in `recv()` a coda
  vuota tornerebbe Ready e si ri-bloccherebbe senza mai leggere la porta
  0x60 (l'hardware resterebbe non drenato). Due tag distinti: 0x40 e' il
  flusso applicativo kbd→tty, 0x41 il risveglio kernel→kbd.
- Test t31 (presenza Kbd/Tty + open device); `test-shell.py` ~30/30 prova il
  percorso completo QMP-sendkey → userkbd → tty → shell → VGA.
- Log seriali con timestamp `[s.cs]` stile dmesg (debug facility, seriale only).
