# AGENTS.md - Istruzioni per Agenti AI

## Contesto del Progetto

Velordor è un **microkernel x86_64** scritto in Rust (ADR-0005). Architettura
microkernel: il kernel contiene scheduling, IPC, gestione della memoria e
routing degli interrupt; driver e servizi (console, file system, devfs, shell)
sono processi userspace che comunicano via IPC per nome con reply implicita,
supporto async e trasferimento dati zero-copy su ring SPSC per-processo. Lo
scheduler è unico RT a 32 priorità con Constant Bandwidth Server (CBS) per la
bandwidth reservation.

**Obiettivo**: un microkernel con isolamento dei servizi in userspace, IPC ad
alte prestazioni (per-nome, async, zero-copy) e CPU time garantito sotto carico.

**Orizzonte (dichiarato, non roadmap)**: self-hosting — un Velordor capace di
ricompilare se stesso. Non pianifica nulla da solo, ma ordina le priorita'
(storage veloce prima, servizi fuori dal kernel poi, personalita' per software
reale dopo). Stato e backlog vivono in `ROADMAP.md` (sorgente unica: tabella
completate, voci PIANIFICATE con scope e vittoria dichiarati, "Parcheggiate"
per le idee senza trigger); la storia dettagliata in
`docs/src/14-cronologia-fasi.md`.

## Stack Tecnico

| Componente | Scelta |
|------------|--------|
| Architettura | Microkernel (ADR-0005) |
| Language | Rust nightly (x86_64-unknown-none) |
| Bootloader | stub PVH custom (kernel/src/boot.asm), niente GRUB |
| Testing | QEMU (qemu-system-x86_64, `-kernel` + nota PVH) |
| Documentazione | mdbook |
| Target | x86_64 bare-metal |

NB: `vga.rs`/`serial.rs` sono DEBUG FACILITY temporanee in-kernel; migreranno
al console server userspace quando esiste l'IPC (ADR-0005 §3).

## Build Commands

```bash
# Build kernel + boot in QEMU (PVH)
./run.sh

# Build kernel only (release)
cargo build --release

# VGA visibile in locale
RUN_DISPLAY=gtk ./run.sh

# Build documentation
cd docs && mdbook build

# Serve documentation locally
cd docs && mdbook serve
```

## Coding Conventions

1. **Sempre** usare `#![no_std]` e `#![no_main]` nei moduli kernel
2. **Volatile** per tutti gli accessi MMIO (VGA, LAPIC, ecc.)
3. **Spin locks** per sincronizzazione (no std::sync)
4. **`hlt`** negli idle loop (mai `loop {}` vuoto)
5. **EOI** sempre dopo interrupt handlers
6. **Comments** solo quando necessario (il codice deve essere auto-esplicativo)
7. **Naming**: snake_case per funzioni/variabili, PascalCase per tipi
8. **Error handling**: usare `Result<T, E>` dove possibile, `unwrap()` solo in init
9. **Stratificazione `libr` (ADR-0025)**: meccanismo (`sys/ipc/heap/task/...`,
   neutro) vs personalita' POSIX al bordo (`posix::to_errno`, `stdio`,
   redirect) vs misti dichiarati (`fs/spawn/print/args`); `Error` vive nel
   modulo neutro `error`. REGOLA: il meccanismo non usa mai `posix::` (vale
   anche per il codice nuovo: non stare in due scatole — verifica con
   `rg "posix::" libs/libr/src`).

## File Structure

```
velordor/
├── kernel/         # Il kernel stesso
│   ├── src/
│   │   ├── main.rs     # Entry point Rust (rust_main)
│   │   ├── boot.asm    # Stub PM32 -> long mode (NASM)
│   │   ├── vga.rs      # VGA text mode
│   │   ├── serial.rs   # UART debug
│   │   └── ...         # Altri moduli
│   ├── Cargo.toml
│   └── linker.ld       # Include la nota PVH per il boot QEMU
├── libs/
│   └── libr/           # libreria di sistema condivisa (userland + testland)
├── syscall-numbers/    # Costanti syscall + costanti condivise (kernel+user)
├── scripts/
│   ├── boot.asm        # MBR 16-bit (riserva, non usato dal path PVH)
│   ├── build_common.sh # build_one() condivisa (freestanding PIC)
│   ├── build-userland.sh  # binari "utente" -> userland/build
│   ├── build-tests.sh  # binari test suite -> testland/build
│   └── putc16.inc
├── run.sh              # build userland + testland + kernel + QEMU (PVH)
├── userland/           # SOLO binari ad uso utente: init, console server,
│   │                   #   fs server, devfs, shell, uptime, kbd/tty (Fase 15),
│   │                   #   disk server (Fase 16); futuri: utility (Fase 18)
│   └── build/          # output .bin dei servizi utente
├── testland/           # TEST SUITE + repro + demo (nessun binario "utente")
│   │                   #   demo, testfs, testfat, hogheap, devreader,
│   │                   #   usertests (+ helper usertest-client/usertest-spin);
│   │                   #   srv/cli: demo storiche Fase 7 NON piu' buildate
│   │                   #   (basate su IPC per PID, rimosse in Fase 12)
│   └── build/          # output .bin dei test
├── docs/               # Documentazione mdbook
│   ├── src/            # Capitoli (mdbook src) + adr/ (decisioni, source unica)
│   │                   # + 14-cronologia-fasi.md (storia dettagliata per fase)
│   ├── book.toml
│   └── book/           # output build mdbook
├── ROADMAP.md          # Stato (tabella completate) + Pianificate + Parcheggiate
└── AGENTS.md           # Questo file
```

**Layout moduli (Cleanup)**: ogni crate ha `main.rs`/`lib.rs` sottile (solo attr, `mod`, import, `_start`/panic o re-export) + moduli tematici (`userfs`: mount/ramfs/ftable/rights/rings/handlers/server; `libr`: ipc/spawn/sys/print/tsc/fs/* + facade che riesporta tutti i path `libr::X`; kernel: `sched_rt/`, `syscall/`, `vmm_user/` con facade e `main.rs` intoccato); i figli usano `use super::*;` (+`use crate::*;` se annidati) e i cross-riferimenti sono path espliciti (`handlers::handle_open`), mai glob dai parent. Costanti/tag condivisi stanno in `syscall-numbers` via `libr`, mai duplicati nei crate.

## Stato corrente

Gate: `[testfs] PASS 5/5` + `[testfat] PASS 7/7` + `[usertests] PASS 57/57` + shell, zero FAIL/PANIC/FAULT (vedi `docs/src/11-testing.md`).

- **Stato e futuro**: `ROADMAP.md` (sorgente unica: tabella completate 1-49, Pianificate, Parcheggiate).
- **Storia dettagliata**: `docs/src/14-cronologia-fasi.md` (log per fase: decisioni, bug trovati, lezioni, validazioni).

## Important Notes

- **Non usare** `std` - solo `core` e `alloc`
- **Testare sempre** con QEMU prima di commit
- **Documentare** ogni decisione architetturale in ADR
- **Aggiornare** `ROADMAP.md` quando si chiudono o aggiungono fasi (stato +
  Pianificate/Parcheggiate); la storia dettagliata va in
  `docs/src/14-cronologia-fasi.md`, mai in questo file
- **Checklist di fine fase (docs anti-marcio)**: nello stesso commit della
  fase aggiornare `docs/src/11-testing.md` (gate corrente + riga test se la
  suite cresce), `docs/src/06-syscalls.md` (tabella numeri + wrapper se ci
  sono nuove syscall), `docs/src/SUMMARY.md` (se ADR/capitoli nuovi),
  `run-tests.sh` (commento gate), `ROADMAP.md` (riga stato) e
  `docs/src/14-cronologia-fasi.md` (voce di fase); i gate delle
  fasi passate restano snapshot storici (mai "corretti" al nuovo totale).
  Un solo gate corrente: `11-testing.md` + AGENTS Testing + `run-tests.sh`.
- **Crate consentite**: solo `no_std`-compatible
- **Kernel higher-half: FATTO (ADR-0020, Fase 27)** — kernel a `-2G+1M`
  (`0xFFFF_FFFF_8010_0000`, LMA 1M) + direct map `[0,64G)` a pagine 2M a
  `0xFFFF_8880_0000_0000`; `PML4[0] = 0` a runtime (NULL-deref faulta).
  Conversioni via `kernel/src/addr.rs` (`phys_to_virt`/`virt_to_phys`/
  `kern_*`); RSP0 e stack su VIRT alte. Dettagli e trappole in `04-memory.md`.
- **TSS per-processo** (ADR-0006): ogni processo ha il proprio TSS con I/O
  bitmap. I driver userspace dichiarano le proprie porte in `io_ranges`
  (`user_binary.rs::NAMED_BINARIES`); chi non ha range non tocca porte.
- **IPC per nome: registry + channel nel kernel** (ADR-0008, Fase 12): i peer
  non si indirizzano piu' per PID. Ogni processo parla su un **`Channel`**
  (coppia bidirezionale creata da `spawn` per i figli, o da `service_lookup`
  per i servizi registrati per nome con `service_register`). Il canale 0 = il
  parent. `libr` risolve `Fs`/`Console`/`Devfs` per nome. I messaggi viaggiano
  per channel_id; `reply` e' implicita al messaggio corrente (via `reply_chan`),
  mai per PID. La morte di un endpoint invalida i suoi canali e libera lo slot
  servizio → riavvio/riuso sicuri. Lo slot canale 0 NON si assegna mai (id 0 =
  sentinella `CHANNEL_PARENT`: assegnarlo faceva risolvere i messaggi al parent
  sbagliato).
- **Hardening (Fase 35, ADR-0026 + Fase 36, ADR-0027)**: cancelli per un avversario "programma
  locale malevolo" — (1) `kill` solo parent/init (i test di restart guidano il
  caos via `init_bounce`, non killano i server direttamente); (2) i servizi di
  sistema si registrano solo da figli di init (`Test` aperto per la suite);
  (3) `map_physical`/`map_in` solo frame del sistema (ring/scratch/VGA), mai
  RAM arbitraria; (4) `FS_REGISTER` solo prefix sotto `/dev/`, replace di un
  driver vivo solo da init-child o dallo STESSO binario (`SYS_PEER_PID` 46 per
  attribuire, `SYS_PEER_INFO` 47 per l'identita'). Strato 2 (Fase 36): hash
  FNV-1a nel PCB misurato allo spawn, manifest generato a build-time verificato
  da init pre-spawn; il kernel resta neutro (ADR-0025: POSIX e' personalità, non
  struttura). Il manifest esclude i binari che lo incorporano (userinit/userfs:
  hash di sé = ciclo instabile, mai fixpoint).
- **IPC reply implicita**: la reply del server va al peer del canale del
  messaggio correntemente elaborato (fissato da `recv` in `reply_chan`), non
  all'ultimo `send`. Piu' client concorrenti su un server sono quindi
  supportati (fix 9.2.2 generalizzato ai canali). Un request-id esplicito lato
  server / `reply_to` e' rimandato: la Fase 13 (async) usa un request-id come
  campo interno del messaggio, senza toccare l'ABI dei registri.
- **IPC asincrono (Fase 13, ADR-0009)**: `send_async` (33) NON blocca e
  ritorna il `req_id` (>= 1); `recv_nonblock` (34) non blocca. Il `req_id` e'
  un campo INTERNO di `PendingMsg` (signed: >= 0 richiesta, < 0 risposta a
  `-req_id`), assegnato dal mittente via `req_next`. La reply del server resta
  implicita: il kernel alla `reply` guarda il target — `BlockedOnReply` →
  `reply_slot` (sync); altrimenti accoda una risposta con `req_id = -reply_req`
  (async). Trasparente a userfs/console/devfs. Vincoli primo passo: no mix
  sync/async in volo per processo; risposte FIFO (`wait_reply` non riordina);
  FS async = 1 op in volo (guard `FS_PENDING`: il formato frame del ring non ha
  lunghezza payload esplicita); reply async persa se la msg_queue del target
  (8 slot) e' piena (log nel kernel).
- **Process lifecycle (Fase 14, ADR-0010, implementata)**: exit/kill kernel-side
  in DUE tempi — (1) "morte logica" immediata (`Scheduler::terminate`): stato
  `Terminated`, release di CBS/servizi/canali, risveglio dei peer bloccati in
  `send` sincrono verso il morto (`waiting_pid`), cascata sulla discendenza,
  accodamento al reclaim; (2) teardown fisico differito (`drain_reclaim` a inizio
  `on_tick`): stack kernel, slot TSS, address space user (foglie PTE `owned`,
  vedi sotto) → poi notifica `EXIT_NOTIFY` a TUTTI i peer e PID nel free-set.
  Notifica DOPO il teardown: nei loop spawn/exit il pool non si esaurisce. Riuso: PID
  (cap 32 concorrenti), slot TSS, canali (`None`) e server CBS (`None`).
  `kill(pid, code)` (35): killabile qualunque processo user tranne init/kernel/
  self. Morte di init → panic documentato. `usertestcli` ha i modi CHURN/KILLME/
   SRVDIE/SYNCWAIT/MNTDIE/OPENDIE/MAPHAMMER/FLOOD; suite 21/21 → 31/31 (t22 churn riuso+leak, t23
   kill+notifica, t24 notifica unificata async+sync, t25 morte driver +
   re-registrazione, t26 morte client senza close + smoke, t27 init-restart devfs,
   t28 restart userfs end-to-end, t29 map-flap isolation, t30 fairness sotto flood).
  **Notifica unificata (14.10)**: DOPO il
  teardown il kernel notifica TUTTI i peer (non solo il parent), ciascuno sul
  canale che li collegava (`die_peers` nel PCB, max 31); `wait_reply` ritorna
  `WaitReplyError::ServerDied{pid,code}` (mai attesa infinita), `fs_collect`
  filtra per canale (`wait_reply_chan`), `drain_stray` scarta senza reply.
  Semantica "UN peer e' morto" (notifiche stale filtrate per pid/canale);
  `Service::Test` = slot usa-e-getta per t24. Retry/init-restart rimandati.
- **Address space teardown e bit "owned" (Fase 14)**: le PTE delle pagine user
  hanno il bit AVL `0x200` (owned) se di proprieta' del processo (code copiato,
  stack user, ring, heap demand-zero). `map_physical`/`map_in` NON lo settano
  (pagine iniettate: VGA, ring di altri processi, scratch). `teardown_user_space`
  libera solo le foglie owned e le page-table private (entry PML4 diverse da
  quelle del kernel), mai i frame altrui.
- **Ring SPSC per-processo, niente piu' buffer FS** (Fase 10.2, sostituisce
  9.6): ogni processo alloca DUE pagine ring (syscall **`sys_ring_alloc` (26)**,
  che riusa il numero del vecchio `fs_buf_alloc`) mappate a `USER_FS_BUFFER`
  (request) e `USER_RESP_RING` (response), e le registra presso userfs con una
  IPC register-only (`FS_BUF_REG`). Ogni operazione FS = 1 frame nel request
  ring `[tag:4][w0:8][w1:8][payload]` + `send(FS_NOTIFY)`; userfs consuma
  SEMPRE l'intero frame (header + payload) e scrive 1 response frame
  `[result:8][w1:8][payload]` — **ECCEZIONE: per i WRITE remoti userfs NON
  consuma il frame** (dedicato `handle_write_remote`): il payload resta nel
  request ring e il driver (console/devfs) lo legge direttamente (mappato con
  `map_in` (27), mapper generico cross-process) avanzando la tail lui stesso.
  Per i READ remoti il driver scrive il response frame nella response ring del
  client (zero copie in ogni percorso). Ring a pagina singola: dati
  `[0x0000..0xFF8)` = 4088 B, head a `0xFF8`, tail a `0xFFC`; capacity reale
  4087 B (free = CAP-1) → libr splitta read/write > ~4000 B in piu' round
  trip. Il kernel NON e' nel percorso dati; slot (`fs_slots`) e syscall FS
  kernel-side (3-7, 23, 24) rimossi.
- **Registrazione driver via ring**: devfs/console si registrano con
  `FS_REGISTER` (0x30) scrivendo un frame `R_REGISTER` nel proprio request
  ring (NON il tag FS_NOTIFY); userfs legge il prefix dalla request ring del
  driver (primo elemento della coppia `(req, resp)` registrata — attenzione a
  non confonderlo col response ring).
- **Reattivita' shell**: a valle del boot i soli processi `Normal` sono i
  servizi interattivi (console, fs, devfs, shell), tutti bloccati in attesa IPC;
  init resta bloccato in `recv`, `useruptime` e' `Low`. Quantum scheduler = 2
  tick (20 ms). Ordine spawn di init: userconsole per PRIMO (registra
  `Console`, kbd lo raggiunge per nome), userfs SUBITO DOPO (registra `Fs`;
  init attende l'ACK "Fs pronto" via canale di nascita prima di spawnare chi usa
  il filesystem), poi uptime/devfs, quindi i test in SEQUENZA (ognuno atteso
  fino a `TEST_DONE` sul canale di nascita), usershell per ultimo (interattivo).
- **I test girano in sequenza, la shell e' ultima**: usertestfs/usertestfat/
  usertests condividono la ramfs di userfs (path e file di lavoro) e l'output
  seriale; la sequenza rende PID e risultati deterministici. Con il buffer
  per-processo (9.6) la race della vecchia shared buffer e' eliminata (la suite
  t15 churn devfs concorrente gira davvero in parallelo). init spawa i test uno
  alla volta e attende il `TEST_DONE` (canale 0x7E) da ciascuno sul canale di
  nascita prima dello spawn successivo.
- **Processi Low e server idle**: i server Normal (fs/shell) in coda di `recv`
  vuota e senza altri runnable diventano `Ready` in tight-loop (fix anti-
  deadlock); una fascia `Low` non e' quindi schedulabile finche' girano. Per
  questo il test di priorita' e' **High vs Normal** (Low usato solo da
  `useruptime` in boot reale).
- **Robustezza scheduler (fix pre-esistenti)**: (1) i wait in userland NON fanno
  busy-loop su syscall (`get_ticks`) — che maschera gli interrupt (IF=0) e
  affama il timer — ma spin puri IF=1 (shell/console; `usertestspin`/`utcbstest`
  fanno batch da 512 spin puri tra due `get_ticks`); (2) `ipc_recv` controlla
  la coda e marca `Blocked` sotto lo STESSO lock (chiusa la race check-then-
  block / lost-wakeup); (3) i wait da IRQ (kbd) usano `pending_wake`: `wake`
  imposta il flag se il processo non e' ancora bloccato, `block_current` lo
  consuma e non si blocca; `block_current` ripristina `Ready` se non c'e'
  nessun altro runnable.
- **Read: solo i byte restituiti sono significativi**: ogni client ha una
  pagina FS propria (mai riusata da altri, Fase 9.6) → niente residui di slot/
  buffer condivisi che finivano nelle risposte di altri device (il vecchio
  bug `/dev/zero`).
- **Heap on-demand in libr** (single allocator): niente piu' `static [u8; N]`
  nei binari user. `libr/src/heap.rs` e' l'UNICO allocatore (free-list first-fit
  con split+coalescenza) ed espone `#[global_allocator]`; i crate user che
  alloccano non definiscono allocatori propri. L'heap parte vuoto a
  `USER_HEAP_BASE` (= `USER_STACK_TOP`) e cresce via la syscall **`sbrk` (25)**,
  che riserva solo VA (`heap_brk`): le pagine vengono materializzate **lazy** dal
  page-fault handler (demand-zero, come brk/mmap di Linux). Binari
  sensibilmente piu' piccoli (es. userfs 132→66 KiB).
- **Kernel heap riservato nel frame allocator**: la regione di 4 MiB del kernel
  heap deve essere marcata `used` nel bitmap fisico (`phys_mem::reserve` in
  `main.rs`). Senza questa riserva i frame della regione finivano ai processi e
  venivano sovrascritti → corruzione della free-list del kernel heap (alloc
  falliti o hang; il fix e' alla radice dei fallimenti dell'heap lazy).

## Testing

```bash
# Test in QEMU (output seriale, esce con Ctrl-C)
./run.sh

# Verifica build
cargo build --release

# Build dei binari user (test suite inclusa)
./scripts/build-userland.sh && ./scripts/build-tests.sh

# Test selftest (feature flag)
cargo build --release --features selftest

# Verifica la mappa di memoria dinamica a diverse dimensioni RAM
# (QEMU -m 4G/16G/32G: la RAM sale sopra 4 GiB per il PCI hole)
timeout 6 qemu-system-x86_64 -m 4G -display none -serial stdio -no-reboot \
  -kernel target/x86_64-unknown-none/release/velordor-kernel

# Produzione (default): niente test, shell subito usabile
timeout 60 ./run.sh > /tmp/boot.log

# Bench throughput su KVM (Fase 23, mai nel gate): 3 run di riferimento
./scripts/bench.sh > /tmp/bench.log
rg '\[bench\]' /tmp/bench-run1.log /tmp/bench-run2.log /tmp/bench-run3.log

# Suite di regressione (boot): 3 righe PASS attese e ZERO FAIL/PANIC
#   [testfs] PASS 5/5
#   [testfat] PASS 7/7
#   [usertests] PASS 57/57
timeout 150 ./run-tests.sh > /tmp/boot.log
rg '\[testfs\] PASS 5/5|\[testfat\] PASS 7/7|\[usertests\] PASS 57/57' /tmp/boot.log
test "$(rg -c 'FAIL|PANIC|#.* FAULT' /tmp/boot.log)" = "0"
```

## Documentation

- Ogni ADR deve essere nel formato `docs/src/adr/NNNN-title.md` (source unica:
  il libro mdbook li legge da li'; nessuna copia altrove)
- Ogni nuovo componente deve avere documentazione in `docs/src/`
- Aggiornare il SUMMARY.md quando si aggiungono nuovi capitoli
- Usare mdbook per la documentazione pubblica

## Common Pitfalls

1. **Dimenticare EOI**: dopo ogni interrupt, mandare End of Interrupt al PIC
2. **Deadlock con spin locks**: un interrupt handler non può prendere un lock già preso
3. **Dimenticare volatile**: gli accessi MMIO devono essere volatile
4. **Stack alignment**: x86_64 richiede 16-byte alignment per SSE
5. **Busy waiting**: usare `hlt` invece di `loop {}` negli idle loop
