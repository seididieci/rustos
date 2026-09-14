# ADR-0013: Mount espliciti in userspace (16b)

**Status**: Implemented (Fase 16b)
**Data**: 2026-09-14

## Contesto

Dopo la Fase 16 il mount FAT era un binding hardcodato (`/dev/sda→/fat` nel
boot di userfs): nessuna mount syscall, nessun umount, nessuna directory di
mount dinamica. Il kernel non ha stato FS dal 9.6 (dispatch senza open/read/
mkdir: tutto viaggia via IPC da `libr`), quindi una `SYS_MOUNT` nel kernel
inoltrebbe e basta — contro ADR-0005.

## Decisione

La "syscall" mount e' API `libr` su IPC esistente (come `open`/`read`):

- `libr::mount(source, target)` / `umount(target)` + frame `R_MOUNT` (`0x16`,
  payload `"source\0target\0"`) / `R_UMOUNT` (`0x17`) via `FS_NOTIFY`;
  zero cambi kernel.
- userfs tiene `Vec<FsMount>` (binding target → sorgente + istanza) con
  longest-prefix match e attivazione lazy: mount inattivo (disco assente) =
  errore agli accessi, mai shadow in ramfs (stesso contratto di prima).
- Boot dalle spec statiche (`/dev/sda→/fat`) con lo stesso codice dei mount
  dinamici (dogfood); restart userfs = tabella ricostruita dalle statiche,
  dinamici persi (stato runtime, come fd: i client ristabiliscono — stesso
  spirito del re-handshake `FS_BUF_REG`).
- Regole: target assoluto normalizzato (rifiuta root, `.`/`..`, vuoti),
  source solo `/dev/sdX[N]` (handle dal nome, validato dal driver), niente
  doppi mount (replace idempotente), `umount` rifiutato con fd aperti
  (EBUSY via scan `FileTable`), `/` non smontabile, fstype per sniffing
  (solo FAT32: BPB invalida = errore).
- Shell: builtin `mount`/`umount` (+ `help`).

## Permessi: quanto costera' (domanda 16b)

FAT da' solo il flag readonly; Linux finge ownership al mount
(`uid=/gid=/umask=`). Scelta a strati:

- **Strato 0 (dentro)**: `mode` sui nodi ramfs (default 0o666/0o777), mapping
  fisso FAT (0o444/0o555), stringa `opts` nella spec — inerte, zero test
  impattati, evita migrazioni future.
- **Strato 1 (fase piccola, quando serve)**: uid per-canale (userfs conosce
  gia' il peer), check R/W/X, `R_CHMOD` — tutto in userfs+libr, zero kernel.
- **Strato 2 (progetto grosso, rimandato al login boundary)**: credenziali in
  `spawn`/PCB, chi le assegna, bypass-vs-capability per i server (confused
  deputy), gruppi, setuid. Senza confine di fiducia resta teatro: oggi e'
  tutto dello stesso utente.

## Confronto microkernel

MINIX 3 (tabella nel server VFS + reincarnation server che riavvia),
QNX (pathname space separato dai server: il namespace sopravvive ai crash),
L4Re/Genode (mount = configurazione di sessione). La nostra scelta (spec
persistenti + re-apply, check live futuro) segue MINIX; il check live chiesto
in 16b (walker che prova ogni source e marca stale i morti) segue QNX.

## Conseguenze

- Contenitore generico (`FsMount` + `MountedFs`, oggi solo variante `Fat`):
  ext2/ISO9660 aggiungono una variante senza reshuffle.
- Test t33 (mount dinamico + contenuto + busy/umount + error paths);
  suite 32/32 → 33/33.
- Limiti noti: niente fstype esplicito, niente mount annidati complessi,
  niente credenziali, dinamici persi al restart di userfs.
