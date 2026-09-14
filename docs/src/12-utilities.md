# Utilities (shell + utility utente, Fase 17)

## Panoramica

rustOS ha una **shell interattiva** (`usershell`) e vari **servizi
userspace** che eseguono in Ring 3. Tutti usano `libr` come libreria
condivisa.

> **Fase 17**: la shell interattiva esiste gia' (Fase 8/9); il completamento
> della fase (utility "utente" aggiuntive) e' in backlog (vedi AGENTS.md).

## Programmi implementati

### usershell

Shell interattiva con comandi built-in. Legge input da `/dev/input/keyboard`
e scrive output sullo stesso fd (il console server disegna sulla VGA).

| Comando | Descrizione |
|---------|-------------|
| `ls [path]` | Elenco directory (default: `/`) |
| `cat <file>` | Stampa contenuto file |
| `touch <file>` | Crea file vuoto |
| `mkdir <dir>` | Crea directory |
| `mount <src> <tgt>` | Monta un device (es. `/dev/sda`) su un target (Fase 16b) |
| `umount <tgt>` | Smonta un target (rifiutato se busy, Fase 16b) |
| `help` | Mostra comandi disponibili |
| `exit` | Termina la shell |

La shell NON mappa la VGA: tutti i passaggi di input/output avvengono
tramite il device `/dev/input/keyboard`, gestito dal console server
(registrato come servizio `Console`, raggiunto per nome — Fase 12/ADR-0008).

### init

Processo radice (PID 1). Spawna i servizi in ordine e poi esegue i test
in sequenza prima di lanciare la shell (i PID sono indicativi: i peer si
raggiungono per nome/canale, non per PID):

1. `userconsole` — driver VGA + tastiera (servizio `Console`)
2. `userdisk` — disk driver ATA (servizio `Disk`, Fase 16)
3. `userfs` — file system server (ramfs + FAT32 via userdisk, servizio `Fs`)
4. `useruptime` — contatore PIT
5. `userdevfs` — `/dev/null`, `/dev/zero` (servizio `Devfs`)
6. `userkbd`/`usertty` — tastiera + terminale (servizi `Kbd`/`Tty`, Fase 15)
7. Test: `usertestfs` → `usertestfat` → `usertests` (attende `TEST_DONE`)
8. `usershell` — shell interattiva (ultima, dopo la suite)

### Console server (userconsole)

Rendering VGA in userspace. Gestisce:
- Scrittura VGA (testo, cursore hardware CRTC)
- Registrazione device `/dev/console` presso userfs via `FS_REGISTER`

La tastiera è gestita da `userkbd`/`usertty` (Fase 15): input da `/dev/input/keyboard`, echo al shell tramite device path DEV.

### FS server (userfs)

File system server con mount table dinamica:
- `/` → ramfs (BTreeMap, scrivibile)
- `/fat` → FAT32 (read-only, via `userdisk` — Fase 16)
- `/dev` → userdevfs (instradamento IPC)
- mount dinamici via `mount`/`umount` (Fase 16b, [ADR-0013](./adr/0013-mount-syscall.md)):
  tabella `Vec<FsMount>` con longest-prefix, attivazione lazy, re-apply delle
  spec statiche a ogni boot

### DevFS (userdevfs)

Server minimale per device speciali:
- `/dev/null` — read = 0 byte, write = scarta
- `/dev/zero` — read = N byte zeropadded, write = scarta

## Libreria (libr)

Tutti i programmi userspace usano `libr` (`libs/libr/`). Include:
- Heap on-demand (free-list, sbrk syscall 25)
- Wrappers FS su ring SPSC: `open`, `read_fs`, `write_fs`, `close`, `readdir`,
  `mkdir` (+ varianti async Fase 13: `read_async`/`fs_collect`)
- FS init lazy: ring alloc (`SYS_RING_ALLOC`, 26) + handshake `FS_BUF_REG`
- `print`/`println` (write su seriale)
- IPC: `spawn`, `send`/`recv`/`reply` + async (`send_async`, `recv_poll`,
  `wait_reply`); risoluzione servizi per nome (`service_register`/`lookup`)

## Build

```bash
# Build servizi utente
./scripts/build-userland.sh

# Build test suite
./scripts/build-tests.sh

# Build + QEMU (tutto incluso)
./run.sh
```

## Riferimenti

- [Writing an OS in Rust - Testing](https://os.phil-opp.com/testing/)
- [OSDev Wiki - Userspace](https://wiki.osdev.org/Kernel_Type_Userspace)
