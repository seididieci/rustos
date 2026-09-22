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
| `cat <file>` | Stampa contenuto file; senza file legge stdin (`<`, Fase 40.4) |
| `touch <file>` | Crea file vuoto (`O_CREAT`) |
| `mkdir <dir>` | Crea directory |
| `mount <src> <tgt>` | Monta un device/`UUID=`/`LABEL=` su un target (Fase 16b) |
| `umount <tgt>` | Smonta un target (rifiutato se busy, Fase 16b) |
| `echo [args]` | Stampa gli argomenti |
| `clear` | Pulisce lo schermo (form feed, gestito dalla console) |
| `wc <file>` | Conta righe/parole/byte (`l p b nome`; senza file legge stdin, nome `-`) |
| `hexdump <file>` | Dump esadecimale a righe di 16 byte (senza file: stdin) |
| `kill <pid\|servizio>` | Termina un processo (`kill init` rifiutato dal kernel) |
| `cd [dir]` | Cambia directory (sonda con `readdir`, default `/`) |
| `pwd` | Stampa la directory corrente |
| `cp <src> <dst>` | Copia file (client-side: read+write) |
| `mv <src> <dst>` | Sposta file (cp+rm, la sorgente si rimuove solo a copia riuscita) |
| `rm <file>` | Cancella file (`R_DELETE`; su `/fat` rifiutato: niente unlink, fuori scope) |
| `rmdir <dir>` | Cancella directory vuota (rifiutata se piena) |
| `ps` | Tabella processi stile Linux: PID NAME PRIO STATE TIME PARENT (syscall 37, Fase 19.1) |
| `run <path> [args...] [&]` | Lancia un programma via fork+exec (Fase 37.2): path esatto (niente ricerca: `/fat/bin/runhello.bin`, non `runhello`), argv[0] = path digitato; `&` = background (prompt subito), senza = foreground (attende; `[exit N]` se N != 0) |
| `jobs` | Tabella job (`[id] pid P run\|done C cmd`; i finiti restano finche' `wait`) |
| `wait [pid]` | Attende i job (tutti o uno) e li rimuove, stampa `pid P: exit C` |
| `help` | Mostra comandi disponibili |
| `exit` | Termina la shell |

> **Fase 37.2**: job = figli diretti (non-detached: muoiono con la shell);
> uscita via `EXIT_NOTIFY` (nessun `wait` kernel). Il parent carica file+argv
> prima del fork (il figlio ha l'FS avvelenato: solo `exec_image_args`).
> Niente job control interattivo (foreground senza scampo: i longevi con `&`;
> segnali → posix-server futuro).

### Redirect (Fase 40.4)

Sintassi bash-like (ultimo vince per slot; `2>&1` aliasa sullo slot 1 del
*momento*: `> /o 2>&1` manda stderr nel file, `2>&1 > /o` lo lascia al
terminale). `> /f` da sola crea/tronca senza eseguire nulla:

| Sintassi | Effetto |
|----------|---------|
| `cmd > /f` | stdout su file (crea + tronca) |
| `cmd >> /f` | stdout in append (crea se manca) |
| `cmd < /f` | stdin dal file (deve esistere) |
| `cmd 2> /f`, `2>>` | stderr su file / in append |
| `cmd > /o 2>&1` | stdout+stderr nello stesso file |

Errori distinti sul terminale (mai nel file): `no such file or directory`
(`ENOENT`), `is a directory`, `read-only file system`. Gli errori dei builtin
non inquinano mai `>` (sink separato `term_err`); `run` fallito riporta
`[exit N]` sul terminale.

Meccanismo (ADR-0031, modello B): per i builtin la shell apre + `set_stdio`
con restore; per `run` apre + `dup_grant` pre-fork e contrabbanda
`(vfd, nonce)` nell'ultimo argv (magic `0x7f`, hex senza NUL — il kernel
rifiuta code extra); lo startup (`entry!`) fa claim + `set_stdio` e nasconde
la spec ad `args_from_stack`. Data plane sempre diretto (mai relay nella
shell); grant cancellati a morte osservata. Dettagli e alternative scartate
(relay, nonce sul canale di nascita, pipe per i file) in ADR-0031 e AGENTS.

### Limiti onesti (redirect)

- **Niente quoting/escape**: un operatore dentro virgolette viene comunque
  interpretato (parser vero in Fase 41).
- **Niente pipe/heredoc** (`|`, `<<`): Fase 42 (pipe-buffer nel posix-server).
- **`2>&1` ≠ zsh `MULTIOS`**: niente tee, ultimo-vince come bash/POSIX.
- **`<` su device** puo' troncare/EOF subito (solo file testati); `cat`
  di `/dev/zero` non termina (come da file — stesso comportamento).
- **stderr dei figli quasi-muto**: niente in userland scrive fd 2 oggi; `2>`
  su `run` crea il file ma resta vuoto finche' un programma non lo usa.
- **`open(O_CREAT)` crea i padri** (ramfs `find_or_create`, mkdir -p):
  `> /nodir/x` crea `/nodir` invece di `ENOENT` (semantica server
  pre-esistente, fuori scope 40.4).
- **Offset dup copiato, non condiviso** (dup-for-handoff, ADR-0031).

### runhello

Primo programma lanciabile (`userland/runhello`, `/bin/runhello.bin` su disco
— non un servizio: init non lo spawna). Stampa gli argv (uno per riga) su
seriale ed esce 0; con argomento `fail` esce 3 (dopo aver stampato). Con
stdin redirectato stampa anche `runhello: stdin:<byte>` (Fase 40.4d). Serve a
`test-shell.py` come target fg/bg con exit code osservabile.

Line editing: il backspace a riga vuota non mangia il prompt (disciplina di
linea in `usertty`: conta i digitati, ingoia il resto — Fase 18.0).

### Limiti onesti

- **Write su `/fat`, si** (Fase 20, scrivibile write-through): `cp` verso
  `/fat` crea/scrive con persistenza al reboot (ramfs resta volatile). Resta
  rifiutato: `rm`/`rmdir`/`mkdir` su `/fat` (niente unlink, fuori scope).
- **argv ai binari, si** (Fase 37.1/37.2, supera il limite Fase 18): stack
  stile Linux come convenzione di dati neutra, `_start` via macro `entry!`,
  `libr::exec(path, argv)` (il kernel non tocca il FS). I builtin restano
  builtin; i programmi separati partono con `run` (split futuro: ogni `.bin`
  in piu' resta piccolo, ~17 KiB runhello).
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

La tastiera è gestita da `userkbd`/`usertty` (Fase 15): input da `/dev/input/keyboard`, echo su `/dev/console` verso la shell.

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
