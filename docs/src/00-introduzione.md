# Velordor - Introduzione

## Perché questo progetto?

Velordor è un **microkernel x86_64** scritto in Rust (ADR-0005). Il kernel
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

Rispetto a un microkernel "minimo" da manuale, Velordor combina alcune scelte
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
velordor/
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

Stato (tabella completate), futuro (Pianificate) e idee parcheggiate vivono
in `ROADMAP.md` alla radice del repo (sorgente unica); la storia dettagliata
per fase in [Cronologia di sviluppo](./14-cronologia-fasi.md).

## Risorse

- [Writing an OS in Rust](https://os.phil-opp.com/) - Blog di Philipp Oppermann
- [OSDev Wiki](https://wiki.osdev.org/) - Documentazione hardware
- [Intel SDM](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html) - Manuale CPU
- [OSTEP](https://pages.cs.wisc.edu/~remzi/OSTEP/) - Teoria degli OS
