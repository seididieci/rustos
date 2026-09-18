# rustOS - Introduzione

## Perché questo progetto?

rustOS è un **microkernel x86_64** scritto in Rust (ADR-0005). Il kernel
contiene solo scheduling, IPC, gestione della memoria e routing degli
interrupt; driver e servizi (console, file system, devfs, shell) sono processi
userspace che comunicano via IPC. Il sistema nasce come progetto di
ingegneria/sperimentale: le scelte architetturali puntano a isolamento reale
dei servizi, IPC ad alte prestazioni e CPU time garantito, non a un kernel
"minimo di esempio".

## Design goals

- **Isolamento dei servizi**: driver e file system sono processi userspace
  (ADR-0005): il crash di un server non abbatte il sistema e il kernel ha una
  superficie d'attacco minima.
- **CPU time garantito sotto carico**: scheduler RT a 32 priorita' + Constant
  Bandwidth Server (CBS, ADR-0007): un task con riserva riceve i suoi tick nel
  periodo anche con la CPU saturata.
- **IPC ad alte prestazioni**: indirizzamento per nome con reply implicita
  (ADR-0008), percorso async con request-id interno (ADR-0009) e trasferimento
  dati zero-copy su ring SPSC per-processo.
- **Device come file**: i driver registrano un prefix di mount presso il file
  system server; l'accesso passa dal normale percorso `/dev/...`.
- **Zero dipendenze di boot**: avvio via protocollo PVH con stub custom
  (ADR-0004).

## Scelte originali

Rispetto a un microkernel "minimo" da manuale, rustOS combina alcune scelte
distintive (dettagli tecnici in [01-architettura](./01-architettura.md)):

- **scheduler unico RT + CBS**: nessun dual scheduler classico/RT — un solo
  percorso di scheduling a 32 livelli con bandwidth reservation;
- **IPC per nome kernel-side**: registry di servizi + `Channel` nel kernel;
  i peer non si indirizzano per PID e la `reply` e' implicita al messaggio
  corrente;
- **IPC async senza toccare l'ABI**: `req_id` come campo interno del messaggio
  (encoding signed), reply implicita decisa dal kernel sullo stato del target;
- **ring SPSC per-processo**: ogni operazione FS trasferisce i dati tra i ring
  del client e il server senza copie intermedie (niente shared buffer);
- **TSS per-processo con I/O bitmap** (ADR-0006): i driver userspace dichiarano
  le porte I/O che possono toccare a ring 3;
- **boot PVH custom**: ELF64 + nota PVH, entry PM32, zero loader esterno.

## Stack tecnico

| Componente | Scelta | Motivazione |
|------------|--------|-------------|
| Linguaggio | Rust (nightly) | Controllo a basso livello con sicurezza di memoria |
| Architettura | Microkernel x86_64 | Isolamento servizi, stile seL4/MINIX (ADR-0005) |
| Bootloader | Stub PVH custom (`kernel/src/boot.asm`) | Nessuna dipendenza esterna, boot diretto QEMU (ADR-0004) |
| Scheduler | RT 32 priorita' + CBS | CPU time garantito sotto carico (ADR-0007) |
| Testing | QEMU | Emulazione senza hardware reale |
| Documentazione | mdbook | Formato standard per documentazione Rust |

## Prerequisiti

- Rust nightly (via rustup)
- QEMU (`sudo dnf install qemu-system-x86`)

## Come iniziare

```bash
# 1. Installare Rust nightly
rustup install nightly
rustup component add rust-src --toolchain nightly
rustup component add llvm-tools-preview --toolchain nightly

# 2. Installare QEMU
sudo dnf install qemu-system-x86

# 3. Build userland + testland + kernel, avvio in QEMU (PVH)
./run.sh

# 4. Solo build kernel
cargo build --release
```

## Struttura del progetto

```
rustos/
├── kernel/         # Il kernel (src/, boot.asm, linker.ld con nota PVH)
├── libs/libr/      # Libreria di sistema condivisa (userland + testland)
├── syscall-numbers/# Costanti syscall + costanti condivise (kernel+user)
├── scripts/        # Build userland/testland, mkfat, ...
├── run.sh          # Build userland + testland + kernel + QEMU (PVH)
├── userland/       # Servizi utente: init, console, fs, devfs, shell, uptime,
│                   # kbd, tty, disk (tutti supervisionati da init)
├── testland/       # Test suite + repro + demo (usertests, testfs, testfat, ...)
└── docs/           # Documentazione mdbook (src/ = capitoli + adr/)
```

## Fasi di sviluppo

| Fase | Descrizione | Stato |
|------|-------------|-------|
| 1 | Bare metal Hello World (VGA) + boot PVH | ✅ Completata |
| 2 | Memory Map (PVH) + GDT/IDT | ✅ Completata |
| 3 | Interrupt hardware (PIC/PIT/kbd) | ✅ Completata |
| 4 | Frame allocator + heap kernel (dinamico; direct map 64G da Fase 27) | ✅ Completata |
| 5 | Processi + scheduler preemptive | ✅ Completata |
| 6 | User mode (ring 3) + syscall | ✅ Completata |
| 7 | IPC sincrona send/recv ⭐ | ✅ Completata |
| 8 | init + console server | ✅ Completata |
| 9 | File system server via IPC (ramfs/FAT32/devfs/shell, Fase 9.1-9.6) | ✅ Completata |
| 10 | IPC optimizations (ring msg_queue, bitmask pick_next, SPSC ring FS) | ✅ Completata |
| 11 | Scheduler RT a 32 priorita' + CBS (bandwidth reservation) | ✅ Completata |
| 12 | IPC per nome — registry + channel nel kernel (ADR-0008) | ✅ Completata |
| 13 | IPC asincrono: send/recv non bloccanti, request-id (ADR-0009) | ✅ Completata |
| 14 | Cleanup processi: exit/kill, notifica al parent, slot a generazioni (ADR-0010) | ✅ Completata |
| 15 | Keyboard + Terminal server in userspace (sgancio tastiera/VGA) | ✅ Completata |
| 16 | Disk/ATA driver server in userspace (sgancio ATA/FS) | ✅ Completata |
| 17 | Diritti per-canale lato server (capability su IPC) | ✅ Completata |
| 18 | Shell + utility utente | ✅ Completata |
| 19 | Introspezione (`ps`) + metadati (`stat`) | ✅ Completata |
| 20 | FAT32 scrivibile (persistenza, ADR-0016) | ✅ Completata |
| 21 | Servizi caricati da disco via `spawn_image` (ADR-0017) | ✅ Completata |
| 22 | Detach dalla cascata di morte (emendamento ADR-0010 §6) | ✅ Completata |
| 23/24/25 | Baseline + ottimizzazioni throughput (PIO multi-settore, cache settoriale write-through) | ✅ Completata |
| 26 | `async`/`await` in `libr` sopra IPC asincrona (ADR-0019, 4 passi) | ✅ Completata |
| 27 | Higher-half kernel + direct map (ADR-0020: 27.1/27.2/27.3) | ✅ Completata |
| 28 | `mmap` anonimo nel basso canonico (payoff higher-half) | ✅ Completata |
| 29 | Protezioni di memoria (`mprotect`/NX, fault→kill del processo) | ✅ Completata |
| 30 | Memoria condivisa tra processi (`shm_create`/`shm_map`) | ✅ Completata |
| 31 | Loader ELF per-segmento (W^X del binario, ADR-0021) | ✅ Completata |

## Risorse

- [Writing an OS in Rust](https://os.phil-opp.com/) - Blog di Philipp Oppermann
- [OSDev Wiki](https://wiki.osdev.org/) - Documentazione hardware
- [Intel SDM](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) - Manuale CPU
- [OSTEP](https://pages.cs.wisc.edu/~remzi/OSTEP/) - Teoria degli OS
