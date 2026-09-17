# AGENTS.md - Istruzioni per Agenti AI

## Contesto del Progetto

rustOS è un **microkernel x86_64** scritto in Rust (ADR-0005). Architettura
microkernel: il kernel contiene scheduling, IPC, gestione della memoria e
routing degli interrupt; driver e servizi (console, file system, devfs, shell)
sono processi userspace che comunicano via IPC per nome con reply implicita,
supporto async e trasferimento dati zero-copy su ring SPSC per-processo. Lo
scheduler è unico RT a 32 priorità con Constant Bandwidth Server (CBS) per la
bandwidth reservation.

**Obiettivo**: un microkernel con isolamento dei servizi in userspace, IPC ad
alte prestazioni (per-nome, async, zero-copy) e CPU time garantito sotto carico.

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

## File Structure

```
rustos/
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
│   ├── book.toml
│   └── book/           # output build mdbook
└── AGENTS.md           # Questo file
```

## Phase Progress

- [x] Fase 1: Bare metal Hello World (VGA) + boot PVH
- [x] Fase 2: Memory Map (PVH hvm_start_info) + GDT/TSS/IDT + handler eccezioni
- [x] Fase 3: Interrupt hardware (PIC, timer, keyboard IRQ — routing kernel-side)
- [x] Fase 4: Frame allocator fisico + heap kernel (identity map dinamica fino a ~109 GiB)
- [x] Fase 5: Processi + scheduler preemptive (context switch reale, PCB, idle, priorita')
- [x] Fase 6: User mode (ring 3) + entry syscall (4 sotto-fasi)
  - [x] 6.1 Infrastruttura: GDT user segment, TSS RSP0 dinamica, page table per-processo (CR3), kernel stack per processo
  - [x] 6.2 Entry Ring 3: frame CPU user + trampoline IRET, primo processo che gira in ring 3 e viene preemptato
  - [x] 6.3 Meccanismo syscall/sysret: MSR STAR/LSTAR/SFMASK, entry (senza swapgs, rip-relative su PERCPU), handler (getpid/write/exit)
  - [x] 6.4 Embed binario user + demo (getpid + write + busy-loop) e preemption in ring 3
- [x] Fase 7: IPC sincrona send/recv ⭐ (cuore del microkernel)
- [x] Fase 8: init + console server (driver VGA/kbd migrano in userspace)
  - [x] 8.1 init: init e' l'unico processo user che spawna i servizi via
        syscall `spawn` (numero 20); process tree radicata in init (campo
        `parent`). Il kernel spawna solo init. Nota: da Fase 9.5 in produzione
        init spawna solo i servizi (console, fs, uptime, devfs, shell); da Fase
        13 init sincronizza il boot attendendo l'ACK "Fs pronto" da userfs.
  - [x] 8.2 console server: `userconsole` mappa VGA (`map_physical`, syscall 21).
        Tastiera in userspace da Fase 15 (userkbd/usertty): nessun ponte kernel.
        `sys_write` stampa solo su seriale.
  - [x] 8.3 uptime in userspace: processo `uptime` spostato dal kernel in
        userspace (`useruptime`); nuova syscall `get_ticks` (22) che ritorna
        il contatore PIT. Solo `idle` resta nel kernel (Fase 15 ha eliminato
        anche `keyboard`: driver PS/2 in userspace).
- [x] Fase 9: File system server (ramfs -> FAT32) via IPC
  - [x] 9.1 Shared buffer page + ramfs server
    - [x] 9.1.1 Kernel: shared buffer page (phys_mem + vmm_user)
    - [x] 9.1.2 Syscall: OPEN/READ/WRITE/CLOSE/READDIR (kernel dispatch)
    - [x] 9.1.3 userland/fs: ramfs server
    - [x] 9.1.4 libs/libr: wrappers open/read/write/close/readdir
    - [x] 9.1.5 Build script: aggiungere fs
    - [x] 9.1.6 Test: usertestfs (write + read verification)
  - [x] 9.2 FAT32 read-only (ATA PIO driver + BPB parsing)
    - [x] 9.2.1 Kernel: TSS per-processo con I/O bitmap (ADR-0006) — ogni
          processo ha il proprio TSS (pool 32 slot, RSP0 per-processo, bitmap
          I/O); `userfs` abilita solo le porte ATA 0x1F0-0x1F7, 0x3F6-0x3F7.
          `ltr` per-switch con azzeramento del bit busy del descriptor.
    - [x] 9.2.2 Kernel: fix IPC reply routing — la reply va al mittente del
          messaggio correntemente elaborato (`reply_target` impostato da
          `recv`), non all'ultimo `send`; piu' client concorrenti non si
          sovrascrivono.
    - [x] 9.2.3 scripts/mkfat.py: generatore immagine FAT32 (~257 MiB, 2 FAT,
          root cluster, file 8.3, subdir con '.'/'..') validato con fsck.fat.
    - [x] 9.2.4 userland/fs: driver ATA PIO (`io.rs` + `block.rs`) e parser
          FAT32 (`fat32.rs`) generalizzato a qualunque cluster size; mount
          table `/` → ramfs, `/fat` → FAT32; allocatore free-list con
          coalescenza (i transients del parser vanno liberati); fallback
          ramfs-only se il disco e' assente.
    - [x] 9.2.5 run.sh: genera `userland/fs/fat.img` e lo monta con
          `-drive file=...,if=ide`.
    - [x] 9.2.6 Test: usertestfat (readdir /fat, read HELLO.TXT e
          SUB/NOTES.TXT, write su /fat deve fallire) — PASS.
  - [x] 9.3 devfs server separato + IPC routing
    - [x] 9.3.1 userland/devfs: server `/dev/null` + `/dev/zero`, si registra
          presso userfs via syscall FS_REGISTER (prefix in uno slot dedicato,
          nessun uso della shared buffer → no race condition)
    - [x] 9.3.2 userfs: mount table dinamica (`Vec<Mount>`), handler
          FS_REGISTER (tag 0x30), `FsKind::Dev { server_pid }`, remote fd
          table (`BTreeMap<(pid,fd), (server_pid, remote_fd)>`), routing
          open/read/write/close/readdir verso server remoto
    - [x] 9.3.3 Test: usertestfat test 5+6 (/dev/null write+read, /dev/zero
          read 16 zeri) — PASS.
    - [x] 9.3.4 Nessuna modifica al kernel: shared buffer gia' mappata in
          tutti i processi user (`setup_user_memory`).
  - [x] 9.4 Shell integration (ls, cat, touch, mkdir) + terminale VGA unico
    - [x] 9.4.1 Fix mount resolution (longest prefix match per `/dev/input`)
    - [x] 9.4.2 Console server = terminale: UNICO proprietario del VGA
          (echo tasti + output client + cursore hardware CRTC 0x3D4/0x3D5);
          tastiera = device `/dev/input/keyboard` (ring buffer +
          DEV_OPEN/READ/WRITE/CLOSE); layout US (`Us104Key`); mapping tasti
          Enter→`\n` e Backspace→`0x08` dai `RawKey` di pc_keyboard.
          La shell NON mappa il VGA: legge tasti e scrive output sullo stesso
          fd del device (DEV_WRITE disegna sulla VGA).
    - [x] 9.4.3 FS mkdir: syscall 23 + userfs ramfs `FsNode::Dir` +
          `libr::mkdir` (+ test usertestfs Test 4) — PASS.
    - [x] 9.4.4 Shell binary: `usershell` client terminale con
          ls/cat/touch/mkdir/exit/help; output specchiato su seriale.
    - [x] 9.4.5 Integrazione: init spawna usershell, `user_binary.rs`
          (`io_ranges` CRTC per userconsole), build script, test automatico
          `scripts/test-shell.py` (ls/cat/mkdir via monitor QEMU) — PASS.
    - [x] FS_REGISTER via syscall: `SYS_FS_REGISTER` (24) in kernel con slot
          dedicato (come open/mkdir) → registrazione driver senza limite di
          lunghezza prefix ne' race; `libr::fs_register`; devfs/console lo
          usano (rimosso il vecchio encoding del prefix in parole IPC,
          limitato a 8 byte).
  - [x] 9.5 Riorganizzazione alberi + suite di regressione usertests
    - [x] 9.5.1 Split layout: `userland/` = SOLO binari utente (init, console,
          fs, devfs, shell, uptime); `testland/` = test/demo/repro (demo, srv,
          cli, testfs, testfat, hogheap, devreader, usertests + helper);
          `libs/libr` = libreria condivisa; `scripts/build-userland.sh` +
          `build-tests.sh` (via `build_common.sh`), output separati.
    - [x] 9.5.2 Per-processo il binario embedded viene COPIATO in frame privati
          (`user_binary.rs::copy_binary`): mappare gli stessi frame a piu'
          processi condivide .bss/.data mutabili (free-list di libr) → due
          istanze dello stesso binario si corrompevano. Single-instance prima.
    - [x] 9.5.3 `testland/usertests`: suite di regressione 17 test con riga
          riepilogo `[usertests] PASS N/N` — syscall core, heap lazy demand-
          zero (fresco=0), ramfs write-multichunk/mkdir/errori, /dev/null e
          /dev/zero, map_physical aliasing (pagina scratch `MAP_TEST_PHYS`
          a 64M riservata dal kernel), IPC echo + multi-client reply_target,
          devfs concorrente + heap churn, preemption ring-3 (via contatore su
          pagina scratch), priorita' High>Normal. Helper: `usertest-client`
          (ECHO/ZEROREAD/NULLW con handshake OPENED/GO che dal buffer
          per-processo 9.6 non e' piu' necessario per la race, ma resta come
          barriera di coordinamento) e `usertest-spin` (busy a
          budget di tick; stessa bin esposta a priorita' diverse).
    - [x] 9.5.4 init esegue i test in SEQUENZA (spawn e attesa di `TEST_DONE`
          IPC) prima della shell: determinismo di PID/output (la race della
          shared buffer single-page sara' risolta in 9.6, non piu' necessaria).
    - [x] 9.5.5 Retrofit: `[testfs] PASS 5/5` e `[testfat] PASS 6/6`.
          Validazione: 6/6 boot puliti + `test-shell.py` 3/3.
  - [x] 9.6 Buffer FS per-processo + zero-copy IPC (rimozione shared buffer)
    - [x] 9.6.1 Kernel: rimossi `fs_slots` (pool di slot) e la shared buffer
          page unica (`FS_BUFFER_PHYS`, mapping a `USER_FS_BUFFER` nello spawn);
          le syscall FS (3-7, 23, 24) non sono piu' nel percorso dati. Nuove
          syscall: `fs_buf_alloc` (26) alloca/mappa la pagina FS per-processo;
          `map_in` (27) inietta solo pagine FS note nello spazio di un altro
          processo user. `map_physical` ora invalida la TLB (rimap finestra).
    - [x] 9.6.2 libr: le operazioni FS diventano IPC dirette client→userfs con
          lazy-init della pagina (syscall 26) + handshake register-only
          `FS_BUF_REG` (0x31); i wrapper open/read/write/close/readdir/mkdir/
          fs_register scrivono nella propria pagina e leggono da li' i risultati.
    - [x] 9.6.3 userfs: registro client (pid→phys); finestra a `USER_FS_BUFFER`
          rimappata al client corrente (`map_physical`); ramfs/fat leggono e
          scrivono direttamente la pagina del client. Per i device remoti
          userfs inietta la pagina del client nel driver (`map_in`) → anche
          `/dev/*` e' zero-copy (devfs/console scrivono nella pagina del client).
    - [x] 9.6.4 Validazione: 4/4 boot puliti — `[testfs] PASS 5/5`,
          `[testfat] PASS 6/6` (incl. /dev/null + /dev/zero), `[usertests]
          PASS 17/17` (t15 churn devfs concorrente in parallelo, nessuna race),
          `test-shell.py` 3/3.
- [x] Fase 10: IPC optimizations (ispirate a KeuO)
  - NOTA: 10.1.1 (Reply-slot pre-read) e 10.3 (Scheduler scalability) sono
    stati RIMOSSI dalla fase e NON implementati:
      - 10.1.1 leggeva il `reply_slot` senza lock subito dopo lo `switch_to`
        (raw pointer); sebbene corretto su x86 single-core (TSO), non e'
        multicore-safe e si vuole poter scalare a piu' CPU in futuro → saltato.
      - 10.3 (run queues per priorita', lock IPC per-processo, batched
        wakeups) e' stato rimpiazzato da un approccio diverso: nuovo scheduler
        RT a 32 priorita' + CBS scritto DA ZERO in un file separato
        (`sched_rt.rs`), poi consolidato come l'UNICO scheduler → vedi Fase 11.
  - [x] 10.1 Ottimizzare il modello sincrono esistente
    - [x] 10.1.2 Ring buffer per `msg_queue`: sostituire `Vec<PendingMsg>` con
          ring fisso (8 slot) embedded nel PCB, no heap, O(1) enqueue/dequeue
    - [x] 10.1.3 Bitmask `pick_next`: sostituire le 2 `Vec` alloc con `u64`
          bitmask per livello di priorita', `trailing_zeros()` O(1)
  - [x] 10.2 SPSC ring per bulk data (sostituzione completa del percorso FS)
    - [x] 10.2.1 Layout ring page (per direzione, 4 KiB, head/tail in-page):
          dati `[0x0000..0xFF8)` (4088 B), head a `0xFF8`, tail a `0xFFC`;
          posizioni dati e head/tail wrapped `% RING_DATA_CAP` (4088);
          SPSC by construction, no locks. NOTA: la capacity reale e' 4087 B
          (free = CAP-1): read/write > ~4072 B di payload vengono spezzate
          dal CLIENT in piu' round trip (Fase 10.2 chunking, multi-frame).
    - [x] 10.2.2 Syscall `sys_ring_alloc` (numero 26, riusa il vecchio slot):
          alloca/mappa DUE pagine ring (request a `USER_FS_BUFFER`, response a
          `USER_RESP_RING`), ritorna i due fisici via IpcResult; sostituisce
          `sys_fs_buf_alloc`. `sys_map_in` (27) e' RIMASTO come mapper
          generico cross-process (rimossa l'autorizzazione FS-specifica
          `is_known_fs_buf_page`).
    - [x] 10.2.3 Protocollo FS su ring: client scrive un frame `[tag:4][w0:8]
          [w1:8][payload]` nel request ring → `send(FS_NOTIFY)` (tag 0x32);
          userfs legge il frame consumando l'intero frame (header+payload) —
          ECCEZIONE: per i WRITE verso device remoti il frame NON viene
          consumato (dedicato `handle_write_remote`: il payload resta nel
          request ring e il driver lo legge direttamente, mappato via
          `map_in`, avanzando la tail) —, processa e scrive il response frame
          `[result:8][w1:8][payload]` nel response ring; 1 IPC round trip per
          operazione.
    - [x] 10.2.4 libr wrappers: `open`/`read_fs`/`write_fs`/`close`/`readdir`
          /`mkdir` scrivono nel request ring, notificano e leggono il result dal
          response ring. `read_fs`/`write_fs` splittano richieste > ~4000 B in
          piu' round trip (`RING_MAX_PAYLOAD`), per restare sotto la capacity
          del ring (es. /dev/zero legge 4096 B in 2 round trip).
    - [x] 10.2.5 userfs rewrite: legge le richieste dal request ring, processa
          (ramfs/fat locali scrivono il response frame loro stessi per
          read/readdir), per i device remoti inietta entrambi i ring del client
          nel driver (`map_in`): il driver scrive il response frame nella
          response ring del client (zero copie) e userfs fa da relay IPC.
          Registrazione driver: handshake `FS_BUF_REG` (0x31, ring fisici) +
          `FS_REGISTER` (0x30) con frame `R_REGISTER` nel request ring del
          driver (il prefix e' letto da userfs dalla request ring mappata).
    - [x] 10.2.6 Rimosso vecchio percorso: `sys_fs_buf_alloc`, `FS_BUF_PHYS`,
          `alloc_fs_buf_page`, `is_known_fs_buf_page`, fs_slots e syscall FS
          kernel-side (3-7, 23, 24). L'area a `USER_FS_BUFFER` e' ora la
          request ring.
- [x] Fase 11: Scheduler RT a 32 priorita' + CBS bandwidth reservation
  - Motivazione: garantire CPU time anche sotto carico al 100% (es. registrare
    audio senza perdere sample). La priorita' fissa non basta: serve bandwidth
    reservation stile Constant Bandwidth Server (Linux SCHED_DEADLINE, RTEMS,
    Rialto).
  - NOTA (consolidamento): la Fase 11 nasceva come secondo scheduler in
    `kernel/src/sched_rt.rs` selezionato a compile time con la feature Cargo
    `rt_scheduler`, affiancando lo scheduler classico (`sched.rs`, 3 priorita').
    Dopo la validazione su tutta la suite (Fase 13/14, 21/21) lo scheduler
    classico e' stato RIMOSSO: `sched_rt.rs` (file mantenuto con il nome
    "rt") e' esposto come `crate::sched` e resta l'UNICO scheduler, sempre
    attivo (nessun feature flag; CBS e syscall 28-30 sempre disponibili;
    `cbs_server` nel PCB sempre presente). I chiamanti usano `crate::sched::*`
    invariati. Vedi ADR-0007 (aggiornato).
  - [x] 11.1 Infrastruttura: scheduler unico
    - [x] 11.1.1 `kernel/src/sched_rt.rs`: scheduler completo (32 priorita' +
          CBS), esposto come `crate::sched` via `#[path]` in `main.rs`.
    - [x] 11.1.2 Stessa superficie pubblica (init/spawn/create_user/on_tick/
          block_current/wake/ipc_*/exit_current/process_*/IpcResult): i
          chiamanti (main/syscall/user_binary/process) usano `crate::sched::*`
          senza cambiare.
    - [x] 11.1.3 Tipo `Priority`: newtype `u8` 0-31 (0=idle, 31=max) con
          costanti alias (`Priority::High`=31 / `Normal`=16 / `Low`=1 /
          `Idle`=0) per leggibilita' del codice.
    - [x] 11.1.4 Build: singola `cargo build --release` (RT sempre attivo),
          verde sulla suite.
  - [x] 11.2 Run queue per-priorita' a 32 livelli (O(1))
    - [x] 11.2.1 `ready_by_prio: [u32; 32]` (bit i = PID Ready al livello i) +
          `ready_prio_mask: u32` (bit i = livello i non vuoto); set/clear
          ready O(1)
    - [x] 11.2.2 `pick_next` O(1): priorita' piu' alta via `leading_zeros()` su
          `ready_prio_mask`, round-robin interno al livello sul bitmask
          (generalizzazione di 10.1.3 a 32 livelli) con cursore PER LIVELLO
          (`rr_cursor[p]`: la rotazione riparte dal bit successivo all'ultimo
          scelto a quel livello). Un contatore globale condiviso tra
          sottoinsiemi diversi NON e' equo: con cicli IPC deterministici il
          cursore si aggancia in fase e un membro muore di fame per sempre
          (osservato sotto KVM: pid 7 mai scelto in ~1900 pick tra
          {4,7}/{7,8}/{7,9} → tastiera muta; TCG lo mascherava rompendo la
          fase con i pick dei quanti)
    - [x] 11.2.3 Mapping priorita' processi esistenti: idle=0, demo/uptime/
          testspin=1, test/demo=2-5, servizi Normal (console/fs/devfs/shell)=
          16-20, utspin_high/keyboard/urgenti=31; quantum invariato (2 tick)
  - [x] 11.3 Constant Bandwidth Server (CBS): bandwidth reservation
    - [x] 11.3.1 `kernel/src/cbs.rs`: `CbsServer { budget_ticks, period_ticks,
          remaining_budget, deadline, task_pid: Option<usize>, active,
          bandwidth }` + pool limitato (`MAX_CBS_SERVERS`). Parametri in TICK
          (1 tick = 10 ms; es. audio Q=2, P=10 → 20% CPU garantito)
    - [x] 11.3.2 Campo CBS nel PCB (`process.rs`): `cbs_server: Option<usize>`
          (sempre presente)
    - [x] 11.3.3 Contabilita' budget in `on_tick`: decrementa `remaining_budget`
          del server del processo corrente; a 0 → throttled: il processo non
          viene piu' scelto via CBS finche' il budget non e' ripristinato (non
          puo' rubare CPU oltre la quota)
    - [x] 11.3.4 Replenishment: alla `deadline` scaduta budget = Q e deadline +=
          P; il task torna schedulabile via CBS. Tempo CBS non usato (task
          bloccato) NON si accumula: va ai processi fixed-priority.
          `tick_replenish` ritorna uno struct `Replenished` su stack
          (`[usize; MAX_CBS_SERVERS]` + len, bound strutturale: uno slot = un
          pid), mai `Vec`: il percorso gira sotto IRQ timer con il lock
          CBS_POOL trattenuto e non deve toccare il lock dell'heap (versione
          ibrida "no-alloc sui percorsi caldi", primo sito convertito).
          Secondo sito: `sys_write` (fd 1/2) non fa piu' `String::from_utf8_lossy`
          (alloc `count` + free O(n²) a ogni println userspace, con rischio
          OOM/panic su `count` enormi) ma streaming raw a chunk 256 B via
          `serial::_write_bytes` (timestamp dmesg byte-wise, zero alloc,
          byte in = byte sul filo per audit fedele; niente piu' `\n`
          spurio aggiunto dal kernel alle righe utente).
          Audit completo heap kernel (tutti i siti): `processes` pre-allocato
          con `Vec::with_capacity(MAX_PIDS)` a init (unica alloc del Vec;
          `place_process` sovrascrive a PID riusato, push solo in crescita
          ≤32, mai shrink → zero realloc dopo il warmup); `main.rs` Box/Vec
          solo sotto feature `selftest` pre-init; crate esterne mai
          (stati inline / hole-list interna). Invariante osservabile:
          `heap::AuditedHeap` conta byte outstanding + alloc totali
          (2 atomiche/op, nessun lock); riga `[sched] tick=` con sched_debug
          riporta `heap_out=`/`heap_n=` — entrambi piatti post-boot
          (misurato: out=27648, n=1 per tutta la suite incl. churn t22).
          `heap_out` piatto = niente crescita netta, `heap_n` piatto = zero
          allocazioni (non solo zero leak).
          Growth path oltre 32 PID (strutturale in 6 punti: `free_pids: u32`,
          `ready_by_prio: [u32; 32]`, `rr_cursor % 32`, pool TSS 32,
          `HEAP_BRK`/`RING_PHYS` per-pid, `PS_SCAN_MAX=32`): strada A = 32→64
          meccanica (u32→u64, array a 64, GDT regge), tutto resta statico e
          no-alloc (`with_capacity` segue la costante da solo); strada B =
          strutture dinamiche = heap sul solo path spawn (redesign vero,
          solo su pressione reale — oggi ~8 servizi, cap ampiamente libero).
    - [x] 11.3.5 Admission control: un nuovo CBS e' accettato solo se
          `Σ(Qi/Pi) + Q/P ≤ CBS_BW_CAP` (~70%; il resto resta ai fixed-priority)
  - [x] 11.4 Syscall CBS (28-30) + wrappers libr
    - [x] 11.4.1 `SYS_CBS_CREATE (28)` (budget, period) → id server o -1
    - [x] 11.4.2 `SYS_CBS_ATTACH (29)`: lega il server al processo corrente
    - [x] 11.4.3 `SYS_CBS_GET_INFO (30)`: budget/period/remaining/bandwidth
          correnti (debug + test)
    - [x] 11.4.4 libr: wrapper `cbs_create`/`cbs_attach`/`cbs_get_info`
  - [x] 11.5 Test CBS + validazione
    - [x] 11.5.1 Test bandwidth: task "audio" con CBS (Q=3, P=10) + task hog che
          satura la CPU (no CBS) → l'audio completa SEMPRE i suoi 3 tick ogni
          10 (nessun sample perso). NOTA IMPLEMENTATIVA: la misura NON usa piu'
          pagina scratch + busy-loop `get_ticks` del parent (maschera IF=0 e
          affama il timer, vedi AGENTS robustezza scheduler): audio e hog
          contano ciascuno i tick OSSERVATI durante il proprio busy-loop
          (batch da 512 spin puri tra due get_ticks) e li riportano al parent
          con `T_DONE` (w1). Il parent resta BLOCCATO in `recv` (mai spin su
          syscall). Attesi: audio ~60/200 (30%), hog ~243/300 (~70%) →
          check `audio_obs in [40,90] && hog_obs > audio_obs` → PASS.
    - [x] 11.5.2 Test admission control: richiesta oltre il cap (~70%) →
          rifiutata (-1) (80% singola e 75% cumulativa rifiutate; 5%+10%
          accettate)
    - [x] 11.5.3 Validazione: suite completa verde (boot pulito +
          `[testfs] PASS 5/5` + `[testfat] PASS 6/6` + `[usertests] PASS 21/21`
          + `test-shell.py` 3/3). NOTA: fix CBS importante — a `exit_current`
          il server CBS legato al processo viene RILASCIATO
          (`cbs::release_pid`): senza, `tick_replenish` risvegliava il processo
          Terminated (`set_ready`) e il scheduler lo riprendeva nel `hlt` di
          exit con IF=0 → congelamento. In piu', `on_tick` ri-aggiunge in ready
          SOLO processi `Ready`/throttled, mai `Terminated`/`Blocked`.
- [x] Fase 12: IPC per nome — registry + channel nel kernel (ADR-0008)
  - Motivazione: l'IPC sincrono per PID (Fase 7) accoppiava i peer al numero di
    processo (`FS_SERVER_PID=4` hardcodato, `CONSOLE_PID`, figli che deducono il
    padre da `cfg.sender`), rendendo fragile riavvio servizi e futura pulizia.
  - [x] 12.1 Registry nel kernel: `enum Service` nel crate `syscall-numbers`
        (`#[repr(u64)]`, discriminant = slot); tabella slot nel kernel
        (`channels.rs`). Nessun servizio ring-3 (bootstrap/latenze).
  - [x] 12.2 Oggetto `Channel` (pool statico): coppia bidirezionale tra due
        processi. I messaggi viaggiano per `channel_id`, mai per PID. La morte
        di un endpoint invalida i canali (`invalidate_pid`) e libera lo slot
        servizio di cui era owner (`release_service`).
  - [x] 12.3 Canale di nascita: `spawn` crea il canale tra parent e figlio; il
        figlio lo usa come canale 0 (= parent), il parent riceve l'handle da
        `spawn`. Elimina il PID dall'IPC padre-figlio.
  - [x] 12.4 Syscall: `service_register` (31), `service_lookup` (32);
         `send`/`recv`/`reply` indirizzano per channel. La `reply` e' implicita
         al messaggio corrente (via `reply_chan`, generalizzazione del fix
         9.2.2). Niente request-id esplicito lato server / `reply_to` in Fase 12
         (vedi ADR-0008: l'ABI a tupla SysV a 6 registri per un request-id di
         ritorno rompeva l'inlining → write a 0x0). La Fase 13 (async)
         introduce un request-id come CAMPO INTERNO del messaggio (non nei
         registri di ritorno), quindi senza problemi ABI.
  - [x] 12.5 kbd (kernel) risolve `Console` per nome e inietta i scancode sul
        canale; niente piu' `CONSOLE_PID`.
  - [x] 12.6 Migrazione userland: fs/console/devfs fanno `service_register`;
        libr risolve `Fs` per nome (`fs_chan`, retry di boot con spin IF=1);
        init sincronizza il boot attendendo l'ACK "Fs pronto" da userfs prima
        di spawnare chi usa il filesystem. Rimossi `FS_SERVER_PID` e le demo
        storiche srv/cli (basate su PID dedotto) dal catalogo binari.
  - [x] 12.7 Test suite migrata a canali di nascita + reply implicita.
        Regressione: 19/19 (x3) + shell 3/3.
- [x] Fase 13: IPC asincrono (primo passo, additivo) (ADR-0009)
  - Motivazione: l'IPC di Fase 12 e' sincrono: un client ha al piu' 1 richiesta
    in volo per canale (si blocca in `send`). L'async permette piu' richieste
    in volo e prepara un futuro `async/await` in libr. Il request-id esplicito
    era stato rimandato in 12.4 per un problema ABI (6° registro di ritorno →
    tupla SysV non inlinable → write a 0x0): la Fase 13 lo introduce come campo
    INTERNO del messaggio, senza toccare l'ABI dei registri.
  - Decisioni implementate:
    - ADDITIVO: si aggiungono primitive async; il sincrono esistente resta
      intatto (rete di sicurezza 19/19 → 21/21).
    - Encoding signed sul campo `req_id` del messaggio (NON su w0, che porta i
      dati applicativi FS): `req_id >= 0` = richiesta, `req_id < 0` = risposta
      asincrona a `-req_id`. Il segno si legge in `recv`.
    - Reply IMPLICITA (nessuna syscall reply_to): il kernel, alla `reply` del
      server, guarda lo stato del target — se bloccato (`BlockedOnReply`) →
      comportamento sync attuale (`reply_slot`); se non bloccato (async) →
      accoda un messaggio-risposta con `req_id = -reply_req`.
    - Vincolo primo passo (rilassabile in futuro): no mix sync/async in volo
      per lo stesso processo; risposte consumate FIFO (server single-threaded
      che risponde in ordine di recv). Miglioramento (reply_to esplicita /
      riordino) in una fase successiva.
    - Syscall nuove: `SYS_SEND_ASYNC (33)`, `SYS_RECV_NONBLOCK (34)`.
  - [x] 13.1 Kernel `process.rs`: `PendingMsg` + campo `req_id: i64` (signed);
        `MsgQueue::try_push` (false se piena → backpressure, oggi push scarta);
        `Process` + `req_next: u64` (contatore req_id per processo) e
        `reply_req: i64` (req_id del messaggio corrente, salvato da `recv`).
  - [x] 13.2 Kernel `sched_rt.rs` (l'unico scheduler, esposto come `crate::sched`):
        - `ipc_send` (sync): assegna `req_id = req_next++` al messaggio.
        - nuova `ipc_send_async`: come ipc_send ma NON blocca il mittente;
          `try_push` al peer; coda piena / canale morto → errore (-1).
        - `pop_msg` condiviso: per le richieste (`req_id >= 0`) salva
          `reply_chan`/`reply_req` ed espone il canale in `rdi`; per le risposte
          async espone il `req_id` negativo in `rdi` (niente reply implicita).
        - `ipc_reply`: se il target e' `BlockedOnReply` → comportamento attuale;
          se non bloccato → accoda `PendingMsg{ req_id: -reply_req, ... }` e
          `set_ready` solo se era `BlockedOnRecv` (coda piena → risposta persa,
          log di warning — limitazione del primo passo).
        - nuova `ipc_recv_nonblock`: come recv ma coda vuota → -1 senza bloccare.
  - [x] 13.3 Kernel `syscall.rs`: dispatch 33/34 + handler `sys_send_async`,
        `sys_recv_nonblock`.
  - [x] 13.4 libr: `IpcMsg` + `req_id` (decodifica dal segno di rdi: per le
        richieste `channel`, per le risposte async `req_id` positivo della
        richiesta originale); `send_async`, `recv_poll()`, `wait_reply(req)`
        (recv bloccante finche' arriva `req_id == req`). Sincrono invariato.
  - [x] 13.5 Demo FS/FAT async = **1 operazione in volo per processo** (il
        formato frame del ring non ha lunghezza payload esplicita → un solo
        frame nel ring alla volta): guard `FS_PENDING` in libr che rifiuta ogni
        altra op FS (sync o async) finche' non si raccoglie; `read_async` /
        `fs_collect` (wait_reply + lettura/consumo del response ring;
        rollback del request ring se `send_async` fallisce). userfs/console/
        devfs INVARIATI (reply implicita); solo fix di commenti obsoleti in
        userfs. Le vere N-in-volo e la backpressure si testano su IPC puro
        verso un server echo (helper usertestcli MODE_SRV).
  - [x] 13.6 Test: usertests t20 (FS async 1-in-volo: read_async hello.txt +
        fs_collect) e t21 (IPC async: N=4 send_async in volo raccolte FIFO +
        backpressure: spam finche' la coda del server, cap 8, e' piena →
        -1 osservato, poi drenaggio). Totale atteso 19/19 → 21/21.
  - [x] 13.7 Verifica: **21/21** (x1) + shell 3/3; zero
        fault/panic; docs 06-syscalls (33/34), 07-ipc (sezione async + vincoli),
        ADR-0009, AGENTS.
  - Limitazioni note (fase futura): mix sync/async sullo stesso canale;
    `wait_reply` assume FIFO (niente riordino locale); wrap di `req_next`;
    reply async persa se la msg_queue del target e' piena.
- [x] Fase 14: Cleanup processi — exit/kill kernel-side + notifica al parent (ADR-0010)
  - Motivazione: chiude il cerchio di IPC per-nome/async. Prima di questa fase
    `exit_current` marcava `Terminated` ma NON liberava stack kernel, slot TSS,
    CR3/address space, page table, ring e heap; i PID non si riusavano (limiti
    strutturali: `ready_by_prio` a 32 bit → max 32 processi pronti, TSS pool 32
    slot monotono, `Vec<Process>` mai compattato, canali `alive:false` mai
    rimossi dal pool). Il parent non veniva mai notificato della morte del figlio.
  - Decisioni concordate (ADR-0010):
    - MODELLO 1 — cleanup kernel-side DIFFERITO: exit/kill marca `Terminated` e
      mette il processo in una coda di reclaim; un passaggio di cleanup (inizio
      `on_tick`, `Scheduler::drain_reclaim`) esegue il teardown (stack kernel,
      slot TSS, address space user: foglie PTE `owned` + page-table private;
      mai le pagine iniettate con `map_physical`/`map_in`). Il rilascio NON
      avviene mai mentre si gira ancora sullo stack del morente. (Non-POSIX:
      nessun obbligo di wait/reap per il parent.)
    - NOTIFICA EXIT UNIFICATA a tutti i peer (con exit code), ma SOLO
      DOPO il teardown: quando un peer si sveglia le risorse sono gia' libere
      → il pool non si esaurisce nei loop spawn/exit. Messaggio `EXIT_NOTIFY`
      (0x7C): w0 = code, w1 = pid, sul canale che collegava ciascun peer
      (il parent e' un peer come gli altri). Consente a init di riavviare i
      servizi morti (restart effettivo rimandato).
    - CASCATA: la morte di un processo (exit/kill) termina TUTTA la discendenza
      (stesso percorso di cleanup, ricorsivo). Morte di init → panic documentato.
    - KILL: syscall `kill(pid, code)` (35, stessa via di exit). Killabile:
      qualunque processo user tranne init, i processi kernel e se stesso (exit).
      Kill esplicito del sottoalbero rimandato alla fase "detach".
    - SLOT A GENERAZIONI/RIUSO: allocatore PID riusabile (bitmask, max 32
      concorrenti); TSS (pool 32, slot 0 boot), canali (slot `None`) e server
      CBS (slot `None`) riusabili. Un PID torna libero solo dopo il rilascio di
      canali/servizi/CBS (fatto a `terminate`) E il teardown (`drain_reclaim`).
    - Detach (futuro, nota): figli che sopravvivono al parent (ri-parentati a
      init) e kill del sottoalbero esplicito.
  - [x] 14.1 `process.rs`: campi `exit_code`, `waiting_pid` (peer su cui un
        `BlockedOnReply` attende la reply, per sbloccarlo alla morte del
        destinatario) e `tss_slot` (slot pool, distinto dal selettore GDT).
  - [x] 14.2 `vmm_user.rs`: PTE bit AVL `0x200` = "owned" (code copiato, stack
        user, ring, heap demand-zero); `map_user_region` = mapping estraneo,
        `map_user_region_owned` = di proprieta'; `teardown_user_space(cr3, pid)`
        walk dal PML4 (salta le entry condivise col kernel U=0) e libera solo le
        foglie `owned` + i frame delle page table private; azzera
        `HEAP_BRK`/`RING_PHYS`.
  - [x] 14.3 `gdt.rs`: pool TSS riusabile (`TSS_FREE` bitmask init in `init`;
        `free_tss_slot(slot)`; `configure_tss` gia' idempotente). FIX: il PCB
        ora tiene lo slot pool (il selettore GDT ha indice base+slot e non
        serviva a liberare il pool).
  - [x] 14.4 `phys_mem.rs`: `free_contiguous(start, n)` per i frame contigui.
  - [x] 14.5 `channels.rs`: `release_pid(pid)` libera gli slot canale (`None`,
        riusabili) e gli slot servizio dell'owner.
  - [x] 14.6 `cbs.rs`: `release_pid` azzera lo slot (prima marcava solo
        `inactive` e saturava il pool di 8).
  - [x] 14.7 `sched_rt.rs`: free-set PID + cap `MAX_PIDS=32`; `terminate`,
        `kill`, coda reclaim fixed-size + `drain_reclaim` in `on_tick`,
        notifica exit in `reclaim_one`, `wake_senders` (morte logica immediata);
        `user_binary.rs::spawn_user` torna `Option` (nessun panic su pool pieno).
  - [x] 14.8 syscall `SYS_KILL` (35) + `libr::kill(pid, code)`.
  - [x] 14.9 Test (in usertests): helper `usertestcli` modi CHURN/KILLME; i loop
        `recv`/`wait_reply` della suite sono EXIT-aware (t13-t21 intatti);
        t22 lifecycle churn (42 spawn/exit ~2 MiB heap → riuso PID, no leak,
        notifiche EXIT_NOTIFY) e t23 kill + exit notify. Suite 21/21 → 23/23.
  - [x] 14.10 Notifica unificata a TUTTI i peer (estensione): `terminate`
        enumera le coppie (peer, channel) e le salva nel PCB (`die_peers`, max
        31 peer distinti = bound provabile, dedupe first-wins); `reclaim_one`
        notifica ogni peer sul suo canale DOPO il teardown (single path, il
        parent e' un peer come gli altri). `libr::wait_reply` ritorna
        `WaitReplyError::ServerDied{pid,code}` (+ `wait_reply_chan` per filtro
        canale, usato da `fs_collect`); `drain_stray` scarta EXIT_NOTIFY senza
        reply. Semantica "UN peer e' morto": notifiche stale filtrate per pid
        (t21/t24) o canale (fs_collect). Nuovo `Service::Test` (slot usa-e-getta
        per t24). Helper SRVDIE/SYNCWAIT; t24 copre path async+sync+slot libero.
        Retry automatico e init-restart rimandati (documentati). Suite → 24/24.
  - [x] 14.11 Cleanup per-peer nei server su EXIT_NOTIFY: userfs purga
        `rings[chan]` + `ftable`/`next_fd[chan]` (con `DEV_CLOSE` best-effort ai
        driver, che restano puliti senza attribuzione) + `mounts.retain`
        (stale first-match avvelenerebbe `resolve_mount` dopo re-registrazione;
        provato: senza retain t25 FAIL); console/devfs skip senza reply (nessuno
        stato per-client: hub-topology; condizione futura documentata). Helper
        MNTDIE/OPENDIE; t25 morte driver + re-registrazione, t26 morte client
        senza close + smoke completo (null/zero/hello/write/mkdir/readdir).
        Suite → 26/26.
  - [x] 14.12 Init-restart + retry client: init supervisiona console/fs/devfs
        (tabella bin/svc/chan/pid; loop EXIT_NOTIFY condiviso con run_test
        cosi' i restart funzionano anche a suite in corso; shell/uptime
        log-only). Respawn + attesa SVC_READY fire-and-forget via send_async
        (tutti i servizi; una send sync resterebbe bloccata — bug trovato:
        hang a boot; wait_ready senza reply + bound 500 tick). Backoff 20 tick
        + hold oltre 3 restart/300 tick. Syscall `service_pid` (36). libr retry
        uniform-retry-once in `fs_send` (re-lookup bounded ~200 tick, caveat
        write at-least-once). userfs replace-on-register. t27 kill devfs →
        sparizione → ricomparsa (pid anche riusato: 6→6 osservato) → operativo.
        Bug trovati: tabella pid allineata prima della registrazione devfs
        (race → attesa READY anche di console/devfs a boot; ACK console subito
        dopo service_register per non fare deadlock con /dev/input). t28
        (restart userfs end-to-end: fixture fresh, wipe probe, /fat persistente,
        /dev operativo) e t29 (map-flap isolation) implementati e PASS. Igiene
        Livello 1: i polling di operativita' in t27/t28 sono throttled (~20 tick
        via `libr::poll_wait`/`open_wait`, mai busy-loop su syscall FS); t30 e'
        gate di fairness scheduler sotto carico IPC (helper FLOOD + latenza mount,
        bound 300, osservato 0-1), NON di saturazione userfs (impossibile con
        client sync: coda 8 slot, <=1 in volo). t31 (Kbd/Tty + device). Lezione
        Fase 15: gli spinner a pari priorita' diluiscono la rotazione (~1
        quantum/hop) — i test attendono BLOCCANDOSI (mai poll aggressivi) e i
        server dormono in recv (event-driven). Rimandate: generazioni PID.
        Suite → 31/31.
  - Verifica: boot pulito, gate `[usertests] PASS 31/31` + shell 3/3, zero
        fault/panic, righe `[reap]` con frame liberi stabili.
- [x] Fase 15: Keyboard + Terminal server in userspace (sgancio tastiera/VGA)
  - `userkbd` (driver PS/2, ring 3): `io_ranges (0x60,0x64)`, init i8042 in
    userland, scancode raw su `/dev/kbd`; servizio `Kbd` (slot 5).
    Svegliato da IRQ1 via wake dell'owner per nome (kernel: solo routing+EOI).
  - `usertty` (terminal server): decode `pc_keyboard` (spostato da console),
    echo su `/dev/console`, byte cotti su `/dev/input/keyboard` (protocollo
    identico: shell INVARIATA); servizio `Tty` (slot 6, solo supervisione).
  - `userconsole`: solo rendering VGA (`/dev/console`, DEV_CONSOLE=3).
  - Kernel: eliminati `kbd_process.rs`, `kbd_events.rs`, `keyboard.rs` + spawn;
    solo `idle` resta oltre init (8.3 superato: niente piu' processi kernel).
  - tty e' client FS PURAMENTE async (mai sync in steady) + EVENT-DRIVEN
    (dorme in recv, wake su KBD_NOTIFY/relay/reply): lezioni apprese —
    (1) ciclo userfs<->driver se il driver blocca su userfs servendo;
    (2) dilution scheduler da spinner (~1 quantum/hop → flooder 25x lento);
    (3) boot async: invio nella stessa chiamata (mai wake atteso pre-send);
    (4) handshake BUF_REG per-canale; (5) open di file device, mai mount-root.
  - Init: spawn console→fs→uptime→devfs→kbd→tty + supervisione kbd/tty; t31
    (Kbd/Tty + open /dev/kbd/kbd + /dev/input/keyboard). Suite → 31/31.
- [x] Fase 16: Disk/ATA driver server in userspace (sgancio ATA/FS)
  - Motivazione: userfs possedeva driver ATA PIO + parser FAT32 con porte
    abilitate solo per lui; ogni read `/fat` bloccava il server nel polling.
  - [x] 16.1 Nuovo `userland/disk`: `io.rs`+`block.rs` da userfs
        (generalizzato a qualunque canale/drive + LBA48 EXT), `detect.rs`
        (reset SRST, probe 2 canali x master/slave via IDENTIFY, ATAPI
        skippato con log, tutto bound), `part.rs` (MBR primarie, graceful se
        assente), `main.rs` (servizio `Disk`=7, `FS_REGISTER` per nodo
        `/dev/sdX`, protocolli `DISK_*`+`DEV_*`, `SVC_READY` pre-mount).
  - [x] 16.2 `userfs` senza ATA: `fat32.rs` generico su trait `BlockSource`,
        nuovo `ipc_disk.rs` (client `DISK_*` sync con riconnessione lazy su
        morte driver); mount con binding `/dev/sda→/fat` (handle 0), fallback
        ramfs-only; open raw `/dev/sdX` via parse nome Linux (rel vuota);
        `EXIT_NOTIFY` invalida il client. Cancellati `io.rs`/`block.rs`.
        Handle codificati `disco<<16|sub` (niente lista nodi).
  - [x] 16.3 Wiring assorbito in 16.2 (da solo lasciava il tree rosso):
        `ATA_PIO_RANGES` (primario+secondario) a userdisk, userfs `&[]`
        (= qualunque `in/out` e' #GP), voce `NamedBinary` + embed, init spawna
        userdisk prima di userfs (+READY entrambi) e lo supervisiona.
        Kernel `ring_alloc`: coppie FRESCHE a ogni chiamata + record
        multi-coppia con free a teardown (mapping non-owned, mai double-free)
        — la cache single-pair aliasava FS/DISK (stesse pagine due volte).
  - [x] 16.4 Test t32 (raw `/dev/sda` con firma boot + kill/restart userdisk
        + smoke `/fat` via riconnessione) + verifica manuale multi-disco
        (secondo `-drive if=ide`: `sdb` + `sdb1` da MBR). Suite → 32/32.
  - [x] 16.5 Docs: ADR-0012 + AGENTS + SUMMARY/00/08/09/11.
  - Bug trovati: (1) deadlock boot da doppia sync incrociata HELLO/
        FS_REGISTER → userdisk MAI sync verso userfs (SM async, nemmeno
        `fs_init`); (2) SM congelata da throttle+recv senza waker → retry a
        ogni wakeup; (3) frame letto a head invece che tail; (4) vedi kernel.
  - Limiti noti (futuro): mount syscall esplicita (16b, FATTO sotto), ATAPI/ISO9660,
    catene extended, scritture disco, caching, DMA+IRQ.
  - Verifica: /fat identica a oggi, testfat 6/6 invariata, suite 32/32.
- [x] Fase 16b: mount/umount espliciti (syscall libr su IPC, zero kernel)
  - Motivazione: dopo la 16 il mount era un binding hardcodato; servono mount
    dinamici (shell, utility future). Il kernel non ha stato FS dal 9.6: una
    SYS_MOUNT inoltrerebbe e basta (contro ADR-0005).
  - [x] 16b.1 userfs: tabella `Vec<FsMount>` + enum `MountedFs` (oggi solo
        `Fat`, domani ext2 senza reshuffle) con longest-prefix + attivazione
        lazy (mai shadow ramfs); boot dalle spec statiche via stesso codice;
        `FsNode` con `mode` + `opts` in spec (placeholder Strato 0, zero
        enforcement); `FileEntry` con mount idx; fix `RamFs::find` (dir a
        singolo componente). Comportamento identico, suite verde.
  - [x] 16b.2 Frame `R_MOUNT` (0x16, "source\\0target") / `R_UMOUNT` (0x17) via
        `FS_NOTIFY`; `libr::mount()/umount()`; handler apply (idempotente) +
        umount con EBUSY (scan fd); shell builtin `mount`/`umount` (+help).
  - [x] 16b.3 Test t33 (mount dinamico + contenuto + busy/umount + error
        paths) + prova shell via monitor. Suite → 33/33.
  - [x] 16b.4 Docs: ADR-0013 + AGENTS + SUMMARY/09/11/12.
  - Permessi (domanda 16b): FAT da' solo readonly; scelta a strati — Strato 0
    dentro (campi+opts), Strato 1 (uid per-canale + check, fase piccola) e
    Strato 2 (identita'/credenziali, progetto grosso) rimandati al login
    boundary. Confronto MINIX (tabella nel VFS) / QNX (namespace separato,
    check live futuro) in ADR-0013.
  - Verifica: testfs 5/5, testfat 6/6, usertests 33/33, zero FAIL/PANIC/FAULT.
- [x] Fase 16c: resolve nome→handle lato driver (single source of truth)
  - Motivazione: il mount indovinava l'handle parsando `/dev/sdX` in userfs
    (`disk_handle`), duplicando la mappa posseduta dal driver (fragile con
    lettere instabili e futuri bus SATA: ogni bus avrebbe richiesto un parser
    nuovo in userfs). Ora mount chiede a userfs, che chiede al driver.
  - [x] 16c.1 Protocollo `DISK_*` centralizzato in `syscall-numbers`
        (HELLO/OPEN/READ/CLOSE + nuovo `DISK_RESOLVE` 0x54); `libr` riesporta.
        userdisk unico owner del servizio `Disk` (opzione d bloccata: SATA
        futuro come backend interno, nessun cambio kernel/ADR-0008).
  - [x] 16c.2 userdisk fonte della verita': tabella `nodes` con handle allocato
        qui; handler `DISK_RESOLVE` (frame `[namelen:8][name]` nel DISK_REQ ring,
        reply w0 = handle o ERR, resync d'epoca come i ring FS). Raw `/dev/sdX`
        (`DEV_*`) intoccato.
  - [x] 16c.3 userfs broker: `IpcDisk::connect` (HELLO + map di ENTRAMBI i ring,
        bound) + `OPEN` a tentativo singolo; `resolve(name)` con un retry solo
        su morte driver (nome ignoto = errore legittimo, mai retry).
        `apply_mount_spec` risolve una volta (fallito = nessun cambio di stato:
        mai spec fantasma, mai distruggere un buon mount); lazy/`reactivate`
        per nome in `resolve_fsmount` e `handle_read`; drop d'epoca su
        EXIT_NOTIFY (fail-loud, mai shadow ramfs, mai handle stale silenzioso).
  - [x] 16c.4 Test t35 (nomi ignoti senza stato, bad-replace innocuo, mount
        valido operativo; t34 resta libero per la Fase 17) + t32 invariato
        (kill/restart ora esercita re-resolve). Suite → 34/34.
  - [x] 16c.5 Docs: emendamenti ADR-0012/0013 + AGENTS + libro (08/09/11).
  - Bug trovati: spec fantasma a resolve fallito (registrava inattiva e
        avvelenava `umount`: t33 "doppio umount accettato") → resolve fallito
        non tocca la tabella; header di t32 mangiato da un edit (ripristinato).
  - Limiti noti (fase futura, mount persistente): lettere ancora instabili
        (ordine di probe), niente UUID/label/serial, mount attivo + reorder
        dopo restart coperto solo via drop+re-resolve per nome (stesso nome).
        → RISOLTI dalla Fase 16d (UUID/LABEL stabili).
  - Verifica: testfs 5/5, testfat 6/6, usertests 34/34 (x2), shell 3/3,
        zero FAIL/PANIC/FAULT.
- [x] Fase 16d: identità stabile disco (UUID/LABEL) + listing sintetizzato
  - Motivazione: dopo 16c il resolve era per nome/lettera `sdX`, ma le lettere
    sono instabili (ordine di probe; un reorder le scambia). Le chiavi stabili
    sono il seriale del volume FAT (vol_id, 4 byte) e la sua label.
  - [x] 16d.1 Decodifica identità: `detect.rs` legge il seriale ATA (IDENTIFY
        word 10-19) in `DiskInfo.serial`; `fat32.rs` espone
        `vol_serial()`/`vol_label_trimmed()`; helper condiviso
        `libr::fat_bpb_identity` (accetta il layout standard firma@66 e quello
        legacy mkfat firma@67).
  - [x] 16d.2 userdisk: tabella `Node { name, handle, vol_uuid, vol_label }`,
        `sniff_identity()` (legge il BPB col proprio driver), registrazione dei
        prefix `/dev/disk/by-uuid/<HEX8>` + `/dev/disk/by-label/<NOME>`;
        `DISK_RESOLVE` esteso a `resolve_node` (nome → UUID → label).
  - [x] 16d.3 userfs: `normalize_source` accetta `/dev/...`, `UUID=<hex8>`,
        `LABEL=<nome>`; `resolve_key`/`resolve_mount_source`; mount statico per
        `UUID=5253544F` (mai piu' `/dev/sda`); open raw by-path risolto dal
        driver (solo per i by-path, non a ogni open di device).
  - [x] 16d.4 Listing sintetizzato: `synth_children` in userfs deriva le voci
        dei padri (es. `/dev`, `/dev/disk/by-uuid`) dai prefix della Mount
        table, senza cambiare il protocollo `DEV_READDIR`.
  - [x] 16d.5 Registrazione multi-prefix ATOMICA: `libr::fs_register_multi`
        (payload NUL-separato) + handler userfs che splitta; devfs registra
        `/dev/null`+`/dev/zero` in UNA sola IPC. FIX deadlock: due register
        sincroni consecutivi creavano un mount forwardable dopo il primo, e se
        userfs stava gia' inoltrando una richiesta al driver (single-threaded,
        `send` bloccante) si incrociava col secondo register → stallo (t30
        sotto flood; tutti i Normal bloccati, solo idle+uptime runnable).
  - [x] 16d.6 `scripts/mkfat.py` parametrizzato (`--serial`/`--label`/
        `--marker`) + layout BPB STANDARD (firma@66): il vecchio layout
        firma@67 faceva sovrapporre `label[0]` al 4° byte del seriale.
  - [x] 16d.7 Test t36 (mount per UUID e per LABEL + contenuto MARKER, open
        raw by-path con firma+seriale, listing `/dev`/by-uuid/by-label) +
        `scripts/test-uuid-reorder.py` (due boot, ordine normale e
        `SWAP_DRIVES=1`: le lettere cambiano, UUID=/LABEL= no). `run.sh` genera
        `fat2.img` (UUID C0FFEE01, label SECOND, MARKER.TXT) e monta due drive.
  - Verifica: testfs 5/5, testfat 6/6, usertests 36/36, shell 3/3, reorder
        PASS, zero FAIL/PANIC/FAULT.
- [x] Fase 17: diritti per-canale lato server (capability su IPC)
  - Motivazione: oggi un `Channel` e' tutto-o-niente (chi ha l'id manda
    qualunque cosa). Il passo verso IPC a capability: diritti attaccati al
    canale, solo in riduzione, senza kernel (userfs conosce gia' ogni peer
    dal canale).
  - [x] 17.0 Protocollo `R_*` centralizzato in `syscall-numbers` (come i
        `DISK_*` in 16c: prima duplicati in libr/userfs/userdisk) + nuovi tag
        `R_RIGHTS_DROP` (0x18) / `R_RIGHTS_GET` (0x19) e bit `RIGHTS_*`
        (OPEN/READ/WRITE/READDIR/MKDIR/MOUNT/UMOUNT, ALL=0x7F — 0xFF con
        `RIGHTS_DELETE` dalla 18.2; niente bit CLOSE: chiudere rilascia stato,
        sempre consentito); `libr` riesporta.
  - [x] 17.1 userfs: tabella `chan → {ops, subtree}` (entry assente =
        `{ALL, root}`, zero alloc); check ops CENTRALE dopo validazione frame
        (a diniego consuma 20+expect + ERR, mai map_in/send — vale anche per il
        WRITE remoto); check subtree alle op con path (OPEN/MKDIR/READDIR +
        MOUNT/UMOUNT-target: estensione ragionata del piano, gli fd restano
        capability pure); DROP (solo shrink AND, widen = nessun cambio,
        subtree vuoto = solo-ops, "/" esplicita da /fat = widen rifiutato) +
        GET self-written `[ops:8][sublen:8][subtree]`; CLOSE/DROP/GET sempre
        consentiti; FS_REGISTER non gatato (handshake server-to-server);
        purge su EXIT_NOTIFY (diritti effimeri, limite dichiarato).
  - [x] 17.2 libr: `rights_drop(mask, Option<subtree>)` / `rights_get(buf)`
        (pattern mkdir + lettura payload intera in stack buffer, mai
        disallineamenti; retry NOHANDSHAKE gratis via `fs_notify_result`).
  - [x] 17.3 Test t34 DIRETTO sul canale di usertests (niente helper: la
        semantica e' "riduco i MIEI diritti"), PER ULTIMO (drop irrevocabili):
        GET default ALL+root, baseline write+read, drop WRITE (write -1/read
        ok), drop MOUNT+subtree /fat (mount -1, open fuori -1, open dentro +
        read + readdir dentro ok, readdir fuori -1, ogni rifiuto seguito da
        op valida = nessun disallineamento ring), widen a root rifiutato +
        GET conferma. Suite → 35/35 (36/36 con t36 della Fase 16d,
        atterrata dopo questa fase).
  - [x] 17.4 Docs: AGENTS (questa voce), ADR-0014, libro (09/11).
  - Bug trovati (grosso, boot): il kernel ingrossato dai binari embedded ha
        spinto il `.bss` (`pit::TICKS` a 0x200320, `_kernel_end` a 0x201000)
        oltre i 2 MiB della boot map → triple fault pre-IDT al primo print
        con timestamp, ZERO output. Fix: boot map a 8 MiB (PD[1..3] large
        page in `boot_tables.rs`, `BOOT_MAP_LIMIT`) + guard fail-loud a inizio
        `rust_main` (`_kernel_end` vs limite, raw serial senza TICKS + `hlt`,
        mai piu' morte silenziosa).
  - Limiti dichiarati (invariati): diritti effimeri (restart userfs =
        re-handshake full); niente policy per-identita' (serve il kernel:
        fase channel-rights); niente revoca selettiva (solo per-morte);
        niente GRANT (canali non trasferibili).
  - Verifica: testfs 5/5, testfat 6/6, usertests 36/36, shell 3/3,
        zero FAIL/PANIC/FAULT.
- [x] Fase 18: Shell + utility utente (era 17, slittata per la nuova 17).
      Decisioni di scoping (prese in pianificazione): builtin nella shell
      (ibrido: split in binari separati solo DOPO l'avvio servizi da disco —
      ogni `.bin` embedded ingrossa il kernel, lezione Fase 17); `rm` con
      nuova op `R_DELETE` (ramfs si, `/fat` rifiutato read-only); cwd lato
      shell si; `ps` (qui marcato "rimandato": richiedeva syscall kernel) e'
      stato poi fatto in Fase 19.1; stretch `ls -l` minimale in Fase 19.2.
  - [x] 18.0 Bugfix backspace mangia-prompt: `usertty::emit` (unico punto che
        genera sia il byte cotto 0x08 in input sia l'eco su console) conta i
        byte digitati sulla riga (`line_len`, reset a `\n` e in
        `reset_to_lookup`); backspace a riga vuota ingoiato (niente in input,
        niente eco). La shell fa gia' pop no-op su String vuota; la console
        resta incondizionata (nessun altro writer emette 0x08). Verifica:
        `test-shell.py` 4/4 (ls, cat, mkdir + backspace via screendump:
        eco `q` visibile, cancel ripristina, 3x backspace a riga vuota = 0
        byte diversi) + gate suite 36/36 invariato. Fix collaterale: KEYMAP
        `"."`: `period` non esiste su QEMU 10.2.2 (`invalid parameter`, tasto
        perso in silenzio) → `dot`; senza, `cat hello.txt` diventava
        `cat hellotxt` (open con O_CREAT crea il file vuoto: silent).
  - [x] 18.1 Builtin senza cambi di protocollo: `echo`, `clear` (nuovo `\x0c`
        in `vga_write_char` console: clear+home), `wc`, `hexdump`, `kill <pid>`
        (via `libr::kill` + `service_pid` per i nomi, `init`→pid 1 diretto:
        non registra il servizio), cwd lato shell (`String`
        + `resolve()` con `.`/`..`) → path relativi per tutti i comandi. Solo
        `usershell` (+ 5 righe console). Dettagli: output a una write per riga
        (ogni write e' un timestamp su seriale: i pezzi non sarebbero contigui
        nel log); `ls` senza args = cwd (non `/`); `cd` sonda con `readdir`
        (mai `open`: creerebbe il file); sorgente `mount` mai risolta
        (`UUID=`/`LABEL=` passano intatti). Verifica: `test-shell.py` 13/13
        (echo, wc `1 4 25 hello.txt`, hexdump `48 65 6c 6c 6f`, cd/pwd/relativi,
        kill errori+init rifiutato, clear via screendump 30060→123 byte accesi
        + shell viva) + gate suite 36/36 invariato, zero FAIL/PANIC/FAULT.
  - [x] 18.1-bis Prompt con cwd: la REPL costruisce `<cwd>$ ` (`$ ` a root);
        nessun impatto sul floor backspace (il prompt non passa da tty::emit).
  - [x] 18.1-ter `ls` mostra i mount: `handle_readdir` fa union di entry
        locali + figli target `FsMount` (nuovo helper, anche inattivi) +
        `synth_children` (rimossa l'esclusione root 16d); dedupe+sort, mai
        shadow (a parita' di nome una sola entry, come `open` driver→FAT→
        ramfs); check subtree Fase 17 invariato (solo nomi, mai contenuti).
        Bug trovato: `is_empty → None` rompeva le dir VUOTE (`cd prova`
        falliva) → flag `exists` separato. Verifica: `ls /` con fat+dev in
        `test-shell.py` (14/14) + gate 36/36 invariato.
  - [x] 18.2 `R_DELETE`: nuovo tag in `syscall-numbers`, handler userfs
        (ramfs `BTreeMap::remove` file/dir-vuote, FAT → ERR read-only, driver
        remoti rifiutati) con check ops+subtree nel choke point Fase 17,
        `libr::remove`, nuovo bit `RIGHTS_DELETE` (incluso in `ALL`, ora
        `0xFF`, per retrocompatibilita': esistenti default-ALL restano pieni),
        builtin `rm`/`mv` (= cp+rm client-side, zero nuove op)/`rmdir`/`cp`.
        Test: ciclo touch/write/rm/read-fail su ramfs + rm su `/fat` rifiutato
        + cp ramfs↔ramfs, da /fat, verso /fat rifiutato.
        Bug veri trovati: (1) `open` creava SEMPRE su ramfs ignorando i flag
        (w1 del frame gia' trasportava i flag, il server li scartava) → ora
        POSIX con `O_CREAT` (0x200) in `syscall-numbers`+`libr`: senza, il file
        deve esistere (`cat` dopo `rm` ricreava il file vuoto: silent!);
        aggiornati i creatori con flag≠O_CREAT (testfs `1`, t7/t26/t34 `0`).
        (2) read a EOF non scrive response frame → torna -1: `cat`/`wc` lo
        tollerano (`n<=0` break), `cp` lo trattava da fatale DOPO copia
        completa → stessa tolleranza (contratto R_READ da pulire in futuro).
        (3) `sendkey` MAIUSCOLE invalide su QEMU (`H`→`invalid parameter`,
        perso in silenzio: 8 tasti invalidi di fila troncavano il comando) →
        combo `shift-x` in KEYMAP. Verifica: `test-shell.py` 24/24 + gate
        36/36 invariato (t7/t26/t34/testfs girano con O_CREAT esplicito),
        zero FAIL/PANIC/FAULT.
  - [x] 18.2-bis Contratto EOF (discusso in pianificazione, alternative
        scartate: remaining in w1 — complessita'/TOCTOU per 1 RT risparmiato
        solo nel caso multiplo-esatto; vedi nota): `handle_read` scrive
        SEMPRE il response frame (anche vuoto) → read oltre EOF torna 0, non
        -1. Round trip invariati (`libr` faceva gia' short-break): cambia solo
        il valore della sonda + file vuoti funzionanti. Zero cambi client/
        protocollo (async gia' compatibile). Test di contratto in t6 (read
        oltre EOF ⇒ 0). Verifica: gate 36/36 + `test-shell.py` 24/24.
  - [x] 18.3 Docs + regressione: capitolo `12-utilities.md` (tabella comandi,
        limiti onesti dell'epoca: no write `/fat` — poi ribaltato dalla Fase
        20; no argv), estensione `test-shell.py`,
        gate invariato 36/36 + shell verde.
- [x] Fase 19: introspezione + metadati (ps/stat).
  - [x] 19.1 `ps` tabellare stile Linux: syscall `SYS_PS_INFO` (37, pattern
        multi-registro `CBS_GET_INFO`: nome 16 B in rdi+rsi, packed
        stato/prio/parent+1/ipc in rdx, tick in r10; -1 se slot vuoto/
        terminato) + snapshot atomico `process_ps` (un solo lock) + contatore
        `ticks_used` nel PCB (incremento in `on_tick` per il current) +
        `libr::ps_info/ps_info::PsEntry` (+ `PS_SCAN_MAX=32` in
        `syscall-numbers`, deve restare = `MAX_PIDS` kernel) + builtin shell
        `ps` (`PID NAME PRIO STATE TIME PARENT`, `run` = se stesso) + t37
        (idle/init presenti parent-None, self Ready, count>=8, TIME init>0 e
        TIME proprio crescente dopo spin puro). Verifica: gate 38/38 +
        `test-shell.py` 30/30, zero FAIL/PANIC/FAULT.
  - [x] 19.2 `stat` lato userfs (zero kernel): frame `R_STAT` (0x1B) con risposta
        self-written `[size:8][kind:8]` (kind=file/dir/device + flag readonly;
        ramfs=len reale, FAT=size da dir entry, **mai readonly** dalla Fase 20,
        device=size 0 readonly 0 senza interrogare il driver) + `libr::stat`/`Stat` +
        check ops+subtree nel choke point Fase 17 (bit `RIGHTS_READDIR`) +
        t38 (ramfs/FAT/device/padri sintetizzati/error paths) + stretch
        `ls -l` minimale (Fase 18 chiusa: `ls [-l]`, riga `tipo size nome[ (ro)]`
        via 1 stat per entry, `? nome` se la entry sparisce in corsa). Verifica:
        gate 38/38 + `test-shell.py` 30/30, zero FAIL/PANIC/FAULT.
- [x] Fase 20: FAT32 scrivibile (persistenza, ADR-0016).
  - [x] 20.0 Protocollo `DISK_WRITE` (0x55): frame `[512:8][settore]` nel
        DISK_REQ ring (handle w0, lba w1), handler userdisk + `node_write`
        (bound check come read), reply senza frame; `IpcDisk::try_write`
        (mirror di `try_read`, un retry solo a canale caduto).
  - [x] 20.1 `write_sector` PIO in `block.rs` (`WRITE SECTORS (EXT)` 0x30/0x34
        + `FLUSH CACHE` 0xE7/0xEA, stesso polling bound dei read) + `outw` in
        `io.rs` + `BlockSource::write_sector` (write-through, niente cache).
  - [x] 20.2 Overwrite entro `size` (`write_file`: read-modify-write a settori
        sul walk catena) in `handle_write_local` (via `fat_mounts`, con
        `reactivate_mount` come il read).
  - [x] 20.3 Crescita + allocazione: `DirEntry`/`FileInfo` con `entry_off` +
        `dir_cluster`, `set_fat_entry` (entrambe le copie, nibble alto
        preservato), `alloc_one` (scan bound `fat_size*128`), `zero_cluster`/
        `zero_range` (mai stale leggibile), `patch_entry` (straddle-safe),
        `fsinfo_bump` (skip se senza firme), `write_grow` (link → zero → dati
        → size per ultima; fallimento alloc = degrado a overwrite; size solo
        di quanto atterrato). Bug veri trovati: (1) restore del test a offset
        EOF appendeva (grow!) avvelenando HELLO per t32-t35 → close+reopen
        prima del restore; (2) size HELLO e' 25 non 26 (contati, niente
        off-by-one di mkfat).
  - [x] 20.4 `O_CREAT` su /fat (`create_file`: 8.3 maiusc, no LFN, attr
        archivio, slot 0x00/0xE5 con crescita dir se piena) + ramo Fat in
        `handle_open`. `mkdir`/`rm` su FAT fuori scope (niente unlink).
  - [x] 20.5 Test + ribaltamenti: `testfat` 7/7 (Test 4 overwrite+restore
        pristino, Test 7 create+grow 9000 B multicluster con pattern e stat;
        handler panic con location); `STAT_READONLY` rimosso per FAT (+ t38,
        `ls -l`, docs); `test-shell.py` ribaltato (cp verso /fat + read-back,
        rm ancora rifiutato, HELLO intatto). Verifica: gate 5/5 + 7/7 + 38/38,
        shell 30/30 in ~3:30 con KVM, `fsck.fat -n` pulito post-sessione
        (6 file, 9 cluster), `mdir`/`mcopy` coerenti.
- [x] Fase 21: servizi da disco (via `spawn_image`, reload sempre da disco).
  - [x] 21.0 `SYS_SPAWN_IMAGE` (38): spawn da byte user + `SpawnMeta` 40 B
        (nome owned 16 B nel PCB, prio 1..31, porte solo init, resto
        `io_count==0`), birth channel condiviso con spawn, bound 256 KiB.
        (ADR-0017)
  - [x] 21.1 init manifest + loader `/fat` (disk/fs embedded, resto da `/bin`,
        test da `/test`; restart rileggono da disco, fail-loud a boot).
  - [x] 21.2 `NAMED_BINARIES` = {init, disk, fs}; boot disk→fs→console (la
        console non puo' piu' essere prima: da disco richiede Fs pronto).
  - [x] 21.3 `scripts/inject-bins.sh` (8.3 senza prefisso `user`, single
        source run.sh/test-shell.py) + Test 1 a 5 entry.
  - [x] 21.4 usertests: helper da `/fat/test` via `spawn_image` + t39
        (`/bin`+`/test` presenti e servizi up). Suite → 39/39.
  - [x] 21.5 Stallo load risolto (t24 35 s, restart 10 s+timeout t27/t30/t32,
        cascata fino al panic init): NON era starvation del pick (contatori
        temporanei: pick equo ~500/testa) ma AMPLIFICAZIONE round-trip ×
        quanti bruciati dagli spinner — un load da 30 KB costava ~480 round
        trip DISK (OPEN per settore + find per read + chunk da 2 KB) e ogni
        handoff attendeva i quanti degli spinner a pari prio. Fix:
        helper sacrificali SRVDIE/KILLME in recv-block (stessi osservabili,
        zero CPU), `spin_ticks` batch 512 in usertests, chunk load 4000 B
        (= RING_MAX_PAYLOAD, init + usertests), cache FileInfo per-fd con
        generazione (bump a ogni mutazione FAT; stat resta sempre fresca:
        niente fd), `IpcDisk` OPEN-once per connessione (re-OPEN solo a
        canale caduto), bound t27 Fase B/C a 2000 (restart-from-disk sotto
        carico misurato ~730 tick). Bug vero trovato: `wait_ready` ingoiava
        le EXIT_NOTIFY altrui → restart persi (userdisk morto durante il
        restart di devfs) → ora stash + drain nei loop (run_test +
        supervisore). "Panic" = solo `init terminato` a cascata (shell
        illeggibile a disco morto), rientrato. Verifica: gate 5/5 + 7/7 +
        39/39 (×2 run TCG completi + ×2 reorder fino a t36 e oltre), shell
        30/30 KVM, reorder PASS, zero FAIL/PANIC.
        Lezione: MAI spinner a pari prio dei server (neanche throttled se il
        carico e' fatto di centinaia di round-trip); i costi si misurano in
        round-trip, non in tick (i tick non sono confrontabili tra TCG/KVM).
- [x] Fase 22: detach dalla cascata di morte (emendamento ADR-0010 §6).
  - [x] 22.0 Flag `SPAWN_FLAG_DETACH` in `SpawnMeta` (1 byte del `_pad`, size
        40 B invariata; bit riservati rifiutati) + `libr::SpawnMeta::detached()`
        builder. Solo lo spawner decide (mai auto-detach, come i diritti che
        si riducono solo); irrevocabile; inerte per i figli di init.
  - [x] 22.1 Kernel: campo `detached` nel PCB; `terminate` salta i detached
        nella cascata e li ri-parenta a init (`parent=1`, log dedicato).
        `kill` invariato (singolo pid + cascata sui non-detached, come
        kill POSIX vs gruppi espliciti): nessuna nuova syscall. Niente fresh
        channel verso init (limite: servira' al protocollo launcher futuro).
  - [x] 22.2 Helper NEST + t40 (MID con 2 KILLME, osservazione via `ps`:
        normale sparita, detached viva parent==1, cleanup-kill). Suite → 40/40.
        Rimandate: generazioni PID complete (cambio protocollo), kill
        sottoalbero oltre la cascata. Verifica: gate 5/5 + 7/7 + 40/40 + shell.
- [x] Fase P0: baseline performance throughput client→block (KVM)
  - [x] P0.1 `libr::rdtsc` + `tsc_calibrate` (TSC in ring 3: CR4.TSD mai
        impostato; calibrazione su PIT ~100 Hz, ~4.45 GHz sul riferimento).
  - [x] P0.2 `testland/bench` (`userbench` → `/test/bench.bin`, 6 op
        end-to-end con warmup, righe `[bench]`): zero_1B (solo IPC),
        sda_512B_seq (IPC+PIO), fat_small_orc (find+IPC+PIO), ramfs_4K
        write/read (FS+IPC), fat_4K_oow (PIO+FLUSH/settore).
  - [x] P0.3 Wiring: feature init `bench` (ortogonale a `skip_tests`,
        `RUN_BENCH=1`, mai nel gate) + `scripts/bench.sh` (N run KVM
        `-accel kvm -cpu host`, fail-loud, timeout atteso). Fix latente in
        `build_common.sh`: `build_one` prendeva solo `$5` (flag multipli
        troncati) → ora `${*:5}`.
  - [x] P0.4 Baseline KVM (media 3 run, stabile ±2%): IPC floor ~1.9 µs/op;
        settore ~1.2 ms (~409 KiB/s); small FAT ~21 ms; ramfs 14-25 µs
        (161-289 MiB/s); overwrite FAT 4K ~64 ms (~62 KiB/s). Collo di
        bottiglia = percorso disco (moltiplicatore settori per op logica),
        non l'IPC. Tabella in `docs/src/13-performance.md`; soglia di
        non-regressione >10%. P1 (PIO multi-settore, flush per richiesta,
        memo FAT intra-op, DISK multi-settore) e P2 (cache/DMA/N-in-volo)
        parcheggiate da analizzare con calma.
- [x] Fase P1: ottimizzazioni throughput (misurate, gate verde)
  - [x] P1.1 userfs-local (nessun protocollo): memo ultimo settore FAT
        (invalida a `set_fat_entry`, drop d'epoca) + read settoriali mirati
        (`read_file` per span, `read_dir` stop a 0x00): small FAT ~21→~2.4 ms.
  - [x] P1.2 DISK multi-settore (frame v2 con count ≤7/IPC, stessi tag):
        `AtaDisk::read/write_sectors` (1 comando PIO per run, 1 flush per
        write), `BlockSource` multi (default loop, `IpcDisk` vero multi),
        `fat32` per run + DEV relay intatto: overwrite 4K ~36→~30 ms.
  - [x] Lezione heap (bug vero): i `Vec` temporanei per-op in userfs
        frammentavano la free-list first-fit (+1 blocco/op FAT → O(n)/O(n²)
        su TUTTE le op dopo: ramfs 25 µs→2 ms). Cura: hot path FAT zero-alloc
        (parse incrementale, run stack ≤8, risposta stack); regola "mai heap
        nel per-op dei server". Diagnostica `libr::heap::heap_stats` mantenuta.
  - [x] Passo A (stesso algoritmo, cliff rimosso): `push_free` inserisce
        ORDINATO per indirizzo e fonde solo coi vicini fisici — free O(n),
        mai O(n²); invariante "lista sempre coalescente" identica, `first_fit`
        invariato. Nessun cambio di semantica di allocazione.
  - [x] Passo C2 (`libr::scratch`, bump + `reset()`, backing `sbrk` dedicato
        fuori free-list, mai liberato, OOM → `None`; align ≤ 8 come heap;
        `alloc_slice` con lifetime di output (coercizione `'static → 's`,
        vincolo `T: 's`) cosi' i contenuti possono prendere in prestito dai
        mount/path senza richiedere `T: 'static`). Migrati in userfs (con UN
        `reset()` in testa al loop; borrow tutti entro l'iterazione): payload
        IPC (il temp piu' grosso, fino a 4096 B/chunk), split path in
        `find`/`find_or_create` (two-pass + slice), check diritti via vista
        borrowed `normalize_sub_view` (zero alloc, owned resta per gli store),
        `synth`/`fsmount_children` → `StrList` in prestito dai mount (+
        adattamento `union`). Restano heap (corretto, A-cheapened): `list_dir`
        del parser, `readdir` ramfs, response `buf`, nomi long-lived
        nell'albero/mount table. Altri server on demand.
  - Regola aggiornata: temporanei per-op su stack o scratch, mai sullo heap
        globale (P1.2 vale ancora per chi non usa scratch).
  - Verifica: gate 5/5 + 7/7 + 40/40 + shell 29/29 (t36 condizionale) KVM,
        bench 3 run stabili, tabella P1 in `docs/src/13-performance.md`.
- [x] Fase P2/C1: cache settoriale write-through in userdisk (ADR-0018)
  - Motivazione: dopo P1 il collo resta il PIO (~1.2 ms/settore); ogni op FAT
    rilegge gli stessi settori (BPB/FAT/dir). Scelta: UN solo strato a blocchi
    nel driver (indipendente dal FS, copre FAT+raw+futuri FS), mai cache file
    in userfs (doppia copia degli stessi 512 B = RAM sprecata).
  - [x] `userland/disk/src/cache.rs`: 256 entry statiche (~128 KiB `.bss`),
        chiave fisica `(disco, lba)`, eviction CLOCK, write-through (prima PIO+
        FLUSH stabili poi update; errore = invalida), zero heap nel per-op
        (array fisso, `static mut` via `addr_of_mut!` per edition 2024),
        contatori hits/misses/inserts + log throttled ogni 2048 accessi.
        Hook futuri senza biforcazioni: `Policy`/`dirty`/`CACHE_SECTORS` in un
        punto solo. I miss contigui restano 1 PIO (`contains` delimita il run,
        `note_misses` conta). `node_read(_multi)`/`node_write(_multi` + relay
        `DEV_*` coerenti per costruzione (stessa chiave fisica).
  - [x] `userfs/fat32.rs`: rimosso `fat_memo` P1.1 (subsumato, un solo strato).
  - [x] Misure A/B stesso host KVM (media 3 run, TSC ~1.6 GHz; la tabella P1 e'
        di un altro host): `fat_small_orc` ~126x (6.1M→49K cyc, 6→838 KiB/s),
        `fat_4K_oow` ~1.9x (86M→46M cyc), resto invariato entro il rumore
        KVM/DVFS (±20-40% sulle op brevi, misurato su run identici). Hit rate:
        bench 74%, suite 91%. Dinamica con reclaim RIMANDATA (sbrk solo cresce,
        nessun canale di pressione kernel→driver); write-back, read-ahead e
        `DISK_STATS` in ADR-0018 come futuri.
  - Verifica: gate 5/5 + 7/7 + 40/40 + shell 30/30, zero FAIL/PANIC/FAULT;
        tabelle P2 in `docs/src/13-performance.md`.
- [x] Fase async/await in libr (ADR-0019, 4 passi verificati uno a uno;
      userdisk rimandato alla fase server-run, vedi Passo 4).
      Sintassi `async/await` (solo `core`) sopra syscall 33/34 invariate, con
      router centrale (l'executor unico a chiamare `recv`, instrada per
      `req_id`; risolve `UnexpectedMsg` per costruzione). Kernel invariato.
  - [x] Passo 1 — `libr::task` (Future `WaitReply`/`RecvMsg`, tratto
        `Receivable` per l'instradamento, Waker no-op, `block_on`, `run`
        const-generic multi-task, pin contenuto, mai heap per-op).
        Nessun chiamante migrato; gate invariato 40/40.
  - [x] Passo 2 — t41 (`block_on` + echo async) / t42 (`run` 2 task +
        `ServerDied`); suite → 42/42 (+ run-tests.sh/testing/docs).
  - [x] Passo 3 — `FsRead` (compone `WaitReply::on_chan`, invio a
        costruzione, collect non-bloccante al poll) sopra read_async/
        fs_collect invariati (prova client reale, protocollo intatto);
        copertura in t20 (doppia lettura, confronto byte).
  - [x] Passo 4 — `Join` (Future+Receivable, delega, annidabile) + t43
        (`Join<Join<W,W>,W>` su 3 server, invii inversi, match per-task);
        suite → 43/43. userdisk NON convertito (rimandato alla fase
        server-run con motivazione: `block_on` nel loop scarterebbe gli
        HELLO/READ sync di userfs → deadlock; serve-while-await richiede
        router anche delle richieste). Insight: con router-esterno solo i
        combinatori trasparenti compongono (blocchi `async` opachi no).
      Vincoli ereditati Fase 13 (non rilassati): no mix sync/async, FIFO,
      FS 1-in-volo. Rimandati: join/select/timeout, rewrite tty/loop,
      rilassamenti formato frame.
- [x] Fase HH: Higher-half kernel + direct map (ADR-0020, H0/H1/H2 verificati
      uno a uno; gate invariato 43/43).
  - [x] H0 — `kernel/src/addr.rs` (`phys_to_virt`/`virt_to_phys`/`kern_*`,
        offset 0) + conversione meccanica di tutti i siti identity (choke
        point entry/set/zero, BITMAP, RSP0=VIRT, boot_info, copy_binary,
        demand-zero, VGA). Zero cambi di comportamento, prova via gate.
  - [x] H1 — il flip: kernel a `-2G+1M` (`0xFFFF_FFFF_8010_0000`, LMA 1M —
        il +1M rende le PD 2M allineate, stile Linux; basi dispari = #PF
        RSVD a zero output, osservato), direct map `[0,64G)` a pagine 2M
        (baseline ogni x86-64: niente PDPE1GB, niente flag QEMU; tetto 64G
        fail-loud oltre), `linker.ld` VMA alte + LMA basse, `boot.asm`
        tutto-alto dual-map (alias LMA linker + EIP reale, `retf`+`movabs`),
        `vmm.rs` ridotto a guard, guard seriali a stadi. Gate-0 `readelf`
        (VMA−LMA == OFFSET + nota PVH) prima di ogni boot.
  - [x] H2 — pulizia e chiusura: stack alto dallo stub, `unmap_low()`
        (`PML4[0] = 0` + flush) a inizio `rust_main` (PML4 user futuri
        ereditano il pulito: nessun walk sui vivi), split VGA UC statico
        (PT 4K per i primi 2M, PAT di reset), selftest NULL-#PF pre-
        preemption (`PML4[0]==0` + fault certificato, run congelata per
        disegno). Bug veri trovati: LMA tabelle nel buco PCI/VGA (solo
        0x90000–0x9FC00 e' RAM: PD direct a LMA fissa 16M verificata
        fail-loud); `MAP_TEST_PHYS` collideva a 16M (t12 scriveva sopra le
        PD → fault ritardato: spostata a 64M + `const assert` di non-
        sovrapposizione). Scoperte: thread di boot mai ripreso dopo il
        primo tick (feature `selftest` post-BOOT_OK + Welcome marciti in
        silenzio — follow-up scheduler, fuori H2); un flake Test-4 FAT
        isolato su TCG (watch item, rerun verde).
- [x] Fase M0: mmap anonimo nel basso canonico (payoff higher-half).
  - Syscall `SYS_MMAP (39)` / `SYS_MUNMAP (40)` + `PROT_*`/`MMAP_FIXED` in
    `syscall-numbers`; zona `[0x10_0000, 0x4000_0000)` (primi 64K mai
    assegnati: NULL faulta); solo anonimo RW in M0 (altro prot/flag = -1).
  - Tabella VMA per-pid (16 record statici, mai heap) in `vmm_user.rs`:
    overlap-check totale, first-fit dal basso, pagine `OWNED` (teardown
    esistente), `munmap` solo VMA intere two-phase, `is_user_range` esteso
    (spawn/write accettano buffer mappati gratis), purge record a teardown.
  - Fault handler: ramo VMA dopo il ramo heap (stesso materializza
    demand-zero). `libr::mmap`/`mmap_fixed`/`munmap`; sbrk/heap invariati.
  - t44 (pattern, multi-PT 3M spot-check, fixed/overlap/len-0/unallineato
    rifiutati, munmap parziale rifiutato senza stato, riuso fixed + zeri
    freschi, write seriale da buffer mappato). Suite → 44/44.
  - Bug vero trovato: `is_user_range` passava `end` invece di `len` a
    `vma_contains_range` (raddoppiava la somma → ogni VMA rifiutata;
    invisibile finche' solo la heap clause serviva). Rimandati: mprotect/NX,
    guard page, file-backed (fase propria: page-in deadlock-prone).

## Important Notes

- **Non usare** `std` - solo `core` e `alloc`
- **Testare sempre** con QEMU prima di commit
- **Documentare** ogni decisione architetturale in ADR
- **Aggiornare** questo file quando si aggiungono nuove fasi
- **Checklist di fine fase (docs anti-marcio)**: nello stesso commit della
  fase aggiornare `docs/src/11-testing.md` (gate corrente + riga test se la
  suite cresce), `docs/src/06-syscalls.md` (tabella numeri + wrapper se ci
  sono nuove syscall), `docs/src/SUMMARY.md` (se ADR/capitoli nuovi),
  `run-tests.sh` (commento gate) e i conteggi qui in AGENTS; i gate delle
  fasi passate restano snapshot storici (mai "corretti" al nuovo totale).
  Un solo gate corrente: `11-testing.md` + AGENTS Testing + `run-tests.sh`.
- **Crate consentite**: solo `no_std`-compatible
- **Kernel higher-half: FATTO (ADR-0020, Fase HH)** — kernel a `-2G+1M`
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
  servizio → riavvio/riuso sicuri.
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
  -kernel target/x86_64-unknown-none/release/rustos-kernel

# Produzione (default): niente test, shell subito usabile
timeout 60 ./run.sh > /tmp/boot.log

# Bench throughput su KVM (Fase P0, mai nel gate): 3 run di riferimento
./scripts/bench.sh > /tmp/bench.log
rg '\[bench\]' /tmp/bench-run1.log /tmp/bench-run2.log /tmp/bench-run3.log

# Suite di regressione (boot): 3 righe PASS attese e ZERO FAIL/PANIC
#   [testfs] PASS 5/5
#   [testfat] PASS 7/7
#   [usertests] PASS 44/44
timeout 150 ./run-tests.sh > /tmp/boot.log
rg '\[testfs\] PASS 5/5|\[testfat\] PASS 7/7|\[usertests\] PASS 44/44' /tmp/boot.log
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
