# Utilities (shell + utility utente, Fase 18)

## Panoramica

Velordor ha una **shell interattiva** (`usershell`) con comandi built-in e vari
**servizi userspace** che eseguono in Ring 3. Tutti usano `libr` come libreria
condivisa. POSIX e' API di `libr`, non ABI del sistema ([ADR-0015](./adr/0015-posix-api-libr-protocollo-interno.md)):
i builtin usano nomi POSIX ma il protocollo userfs sottostante e' interno.

> **Fase 18 completata**: builtin utente (18.1: echo/clear/wc/hexdump/kill +
> cwd; 18.2: rm/cp/mv/rmdir via `R_DELETE` + `O_CREAT` POSIX + contratto
> EOF=0), prompt con cwd (18.1-bis), `ls` che mostra i mount (18.1-ter),
> disciplina di linea anti-prompt-eating in tty (18.0).

## Programmi implementati

### usershell

Shell interattiva con comandi built-in. Legge input da `/dev/input/keyboard`
e scrive output sullo stesso fd (il console server disegna sulla VGA). Tiene
una cwd client-side: i path relativi si risolvono contro di essa, il prompt
la mostra (`/prova$ `, `$ ` a root).

| Comando | Descrizione |
|---------|-------------|
| `ls [-l] [path]` | Elenco directory (default: cwd; mostra anche i mount: `fat`, `dev`). Con `-l` una riga per entry `tipo size nome[ (ro)]` (tipo `d`/`-`/`v`, via `R_STAT`, Fase 19.2) |
| `cat <file>` | Stampa contenuto file |
| `touch <file>` | Crea file vuoto (`O_CREAT`) |
| `mkdir <dir>` | Crea directory |
| `mount <src> <tgt>` | Monta un device/`UUID=`/`LABEL=` su un target (Fase 16b) |
| `umount <tgt>` | Smonta un target (rifiutato se busy, Fase 16b) |
| `echo [args]` | Stampa gli argomenti |
| `clear` | Pulisce lo schermo (form feed, gestito dalla console) |
| `wc <file>` | Conta righe/parole/byte (`l p b nome`) |
| `hexdump <file>` | Dump esadecimale a righe di 16 byte |
| `kill <pid\|servizio>` | Termina un processo (`kill init` rifiutato dal kernel) |
| `cd [dir]` | Cambia directory (sonda con `readdir`, default `/`) |
| `pwd` | Stampa la directory corrente |
| `cp <src> <dst>` | Copia file (client-side: read+write) |
| `mv <src> <dst>` | Sposta file (cp+rm, la sorgente si rimuove solo a copia riuscita) |
| `rm <file>` | Cancella file (`R_DELETE`; su `/fat` rifiutato: niente unlink, fuori scope) |
| `rmdir <dir>` | Cancella directory vuota (rifiutata se piena) |
| `ps` | Tabella processi stile Linux: PID NAME PRIO STATE TIME PARENT (syscall 37, Fase 19.1) |
| `help` | Mostra comandi disponibili |
| `exit` | Termina la shell |

Line editing: il backspace a riga vuota non mangia il prompt (disciplina di
linea in `usertty`: conta i digitati, ingoia il resto — Fase 18.0).

### Limiti onesti

- **Write su `/fat`, si** (Fase 20, scrivibile write-through): `cp` verso
  `/fat` crea/scrive con persistenza al reboot (ramfs resta volatile). Resta
  rifiutato: `rm`/`rmdir`/`mkdir` su `/fat` (niente unlink, fuori scope).
- **Niente `argv` per binari separati**: i comandi sono builtin; `spawn`/
  `spawn_image` passano solo il nome (gli helper di test usano il canale di
  nascita come argv). Lo split in binari separati potra' appoggiarsi
  all'avvio servizi da disco (Fase 21: `spawn_image` da `/bin`+`/test`, solo
  lo storage-TCB e' embedded).
- **`ls -l` minimale**: 1 round trip `R_STAT` per entry (ok per dir piccole);
  niente owner/mtime (`Stat` non li ha); entry sparita tra `readdir` e `stat`
  → riga `? nome`, mai abortito.
- **Read oltre EOF torna `0`** (contratto 18.2-bis); `open` senza `O_CREAT`
  non crea (POSIX, 18.2).

La shell NON mappa la VGA: tutti i passaggi di input/output avvengono
tramite il device `/dev/input/keyboard`, servito dal terminal server
`usertty` (la console `userconsole` fa solo rendering `/dev/console` — Fase 15).

### init

Processo radice (PID 1). Spawna i servizi in ordine e poi esegue i test
in sequenza prima di lanciare la shell (i PID sono indicativi: i peer si
raggiungono per nome/canale, non per PID). Dalla Fase 21 solo disk/fs sono
embedded; gli altri partono da `/fat/bin` via `spawn_image`:

1. `userdisk` — disk driver ATA embedded (servizio `Disk`, Fase 16)
2. `userfs` — file system server embedded (ramfs + FAT32 via userdisk, servizio `Fs`)
3. `userconsole` — rendering VGA da disco (servizio `Console`; da disco non
   puo' essere prima: il load richiede userfs pronto)
4. `useruptime` — contatore PIT
5. `userdevfs` — `/dev/null`, `/dev/zero` (servizio `Devfs`)
6. `userkbd`/`usertty` — tastiera + terminale (servizi `Kbd`/`Tty`, Fase 15)
7. Test: `usertestfs` → `usertestfat` → `usertests` (attende `TEST_DONE`)
8. `usershell` — shell interattiva (ultima, dopo la suite)

### Console server (userconsole)

Rendering VGA in userspace. Gestisce:
- Scrittura VGA (testo, cursore hardware CRTC)
- Registrazione device `/dev/console` presso userfs via `FS_REGISTER`

La tastiera è gestita da `userkbd`/`usertty` (Fase 15): input da `/dev/input/keyboard`, echo alla shell tramite device path DEV.

### FS server (userfs)

File system server con mount table dinamica:
- `/` → ramfs (BTreeMap, scrivibile)
- `/fat` → FAT32 scrivibile (via `userdisk` — Fase 16, scrittura Fase 20)
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
