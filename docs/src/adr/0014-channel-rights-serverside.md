# ADR-0014: Diritti per-canale lato server (capability su IPC)

**Status**: Implemented (Fase 17)
**Data**: 2026-09-14

## Contesto

Dopo la Fase 16c un `Channel` e' tutto-o-niente: chi possiede l'id puo'
inviare qualunque tag a userfs. E' il passo mancante verso IPC a capability.
Il kernel (ADR-0008) conosce i peer ma non i permessi; userfs invece conosce
gia' ogni peer dal canale (`chan = msg.channel`) e ha il punto di choke
ideale (dispatch `FS_NOTIFY` dopo validazione frame, prima di qualunque
contatto handler/driver).

## Decisione

Self-restriction only, senza kernel:

- Tabella `chan → {ops bitmask, subtree}` in userfs. Entry assente =
  `{ALL, root}`: zero alloc, comportamento invariato finche' nessuno droppa
  (tutta la suite esistente resta verde a default pieni).
- Bit `RIGHTS_*` (`syscall-numbers`, riesportati da `libr`): OPEN/READ/WRITE/
  READDIR/MKDIR/MOUNT/UMOUNT, `ALL = 0x7F`. CLOSE senza bit: chiudere rilascia
  stato, sempre consentito (negare la cleanup intrappolerebbe il client).
- Check su due livelli: ops bit CENTRALE dopo validazione frame (a diniego
  consuma `20+expect` + ERR, mai `map_in`/`send` al driver — vale anche per il
  WRITE remoto negato); subtree solo alle op con path (OPEN/MKDIR/READDIR +
  MOUNT/UMOUNT-target: estensione ragionata — gli fd restano capability pure,
  read/write/close non ricontrollano il path aperto).
- `R_RIGHTS_DROP` (0x18): `w0` = mask da tenere, payload = subtree (vuoto =
  solo-ops). Solo shrink (`ops &= mask`), widen = ERR senza nessun cambio
  (prima valida, poi applica); `/` esplicita da `/fat` = widen rifiutato.
  Irrevocabile per disegno: nessun GRANT (i canali non sono trasferibili).
- `R_RIGHTS_GET` (0x19): risposta self-written `[ops:8][sublen:8][subtree]`
  (come read/readdir). DROP/GET sempre consentiti (gestire i propri diritti
  non si nega). `FS_REGISTER` non gatato: handshake server-to-server, fuori
  dal modello self-restriction.
- Tag `R_*` centralizzati in `syscall-numbers` (prima duplicati in
  libr/userfs/userdisk — stessa igiene dei `DISK_*` in 16c).
- Purge su `EXIT_NOTIFY` come rings/ftable: diritti effimeri (restart userfs
  = re-handshake full).

## Limiti dichiarati (non-teatro)

- Niente policy per-identita': senza credenziali nel kernel (fase
  channel-rights futura) ogni restrizione e' auto-imposta; un client
  malevolo semplicemente non droppa. Il valore e' per sandboxing cooperativo
  (helper/test, futuri plugin) e come API pronta.
- Niente revoca selettiva: solo per-morte (esistente, Fase 14).
- Niente GRANT/delega: i canali non sono trasferibili (ADR-0008).
- Mount/umount restano globali: la subtree li vincola solo sul target; la
  confinazione completa richiede il login boundary (ADR-0013 Strato 2).

## Confronto microkernel

seL4 (capability vere nel kernel: mint/copy/revoke), Fuchsia/Zircon (handle
rights attenuabili solo in riduzione — stesso principio del nostro DROP),
MINIX (uid per-canale nel VFS, futuro Strato 1). La nostra scelta e' lo
Zircon-model senza kernel: riduzione-only lato server.

## Conseguenze

- `libr::rights_drop(mask, Option<subtree>)` / `rights_get(buf)` (pattern
  mkdir + retry NOHANDSHAKE gratis); risposta GET letta intera in stack
  buffer (mai disallineamenti ring).
- Test t34 diretto sul canale di usertests (nessun helper), PER ULTIMO in
  suite (drop irrevocabili): GET default, drop WRITE, drop MOUNT+subtree /fat,
  widen rifiutato; ogni rifiuto seguito da op valida (anti-wedge ring).
  Suite 34/34 → 35/35.
- Bug trovato (grosso, boot): vedi Fase 17 in AGENTS.md — boot map 2 MiB
  superata dal `.bss`, fix 8 MiB + guard fail-loud in `rust_main`.
