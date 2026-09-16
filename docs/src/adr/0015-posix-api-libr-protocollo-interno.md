# ADR-0015: POSIX come API di `libr`, protocollo FS interno versionabile

**Status**: Accepted
**Data**: 2026-09-15

## Contesto

`libr` espone gia' nomi in stile POSIX (`open`/`read_fs`/`write_fs`/`close`/
`readdir`/`mkdir`/`mount`), ma il protocollo sottostante e' interamente custom:
frame `[tag:4][w0:8][w1:8][payload]` su ring SPSC, tag `R_*` nostri, handshake
`FS_BUF_REG`, diritti per-canale (Fase 17), mount per `UUID=`/`LABEL=` (Fase
16d). Il kernel non ha alcun concetto di file dal 9.6 (le syscall FS
kernel-side 3-7, 23, 24 sono state rimosse: solo IPC + ring + pagine).

Restava implicito il principio che tiene insieme queste scelte: POSIX e'
l'interfaccia per i programmi, NON il vincolo su come il sistema funziona
dentro. Senza una decisione scritta, ogni estensione futura (errori tipati,
namespace per-processo, signalling non-POSIX) rischia di essere valutata con
il metro sbagliato ("ma POSIX dice..."). Stessa decisione chiude anche la
questione del porting di rust `std` su `libr`.

## Decisione

1. **POSIX e' API di `libr`, non ABI del sistema.** I programmi vedono nomi e
   semantica POSIX-like; sotto, `userfs` e il wire protocol sono liberi di
   evolvere senza alcun obbligo di compatibilita' con lo standard.
2. **Il protocollo userfs e' interno e versionabile.** Frame, tag, handshake,
   layout dei ring e semantica dei mount possono cambiare tra fasi; l'unico
   contratto e' `libr` (ricompilata insieme ai binari via `build_common.sh`).
   Nessun programma parla mai il protocollo direttamente.
3. **Il kernel resta fuori dal filesystem.** Nessun concetto di file, fd, path
   o permessi entra nel kernel: solo canali, messaggi e pagine. Vedi ADR-0005.
4. **Le estensioni non-POSIX sono cittadini di prima classe.** `rights_drop`/
   `rights_get`, mount per `UUID=`/`LABEL=`, `DISK_RESOLVE` vivono in `libr`
   accanto alle wrapper POSIX, senza bisogno di giustificazione POSIX.
5. **Niente porting di rust `std` finche' la superficie non lo richiede.**
   `libr` resta `core`+`alloc` e cresce per necessita' (come gia' fatto con
   `mount`, `rights_drop`, `fat_bpb_identity`). Una `std` piena di stub
   `unimplemented!()` (mancano thread, `fork`/`exec`, rete) darebbe falsa
   compatibilita' e binari gonfi.

## Conseguenze

- La suite `usertests` e' la specifica eseguibile del comportamento: una
  modifica al protocollo che lascia la suite verde e' per definizione
  compatibile.
- Le deviazioni intenzionali da POSIX vanno documentate dove nascono
  (`09-filesystem.md` + ADR della fase), non nascoste: divergenza dichiarata,
  mai accidentale.
- Resta aperta senza strappi la strada a: errori tipati in `libr` (un
  `FsError` al posto del -1 muto), fd per-processo, namespace per-processo,
  `GRANT`/delega dei canali.

## Limiti dichiarati (gap onesti verso una POSIX credibile)

- Niente `errno`: solo -1 generico (primo candidato a colmare il gap, vedi
  sopra).
- `O_CREAT` implementato (Fase 18.2) e `stat` implementata (Fase 19.2,
  `R_STAT` senza open); restano fuori `O_RDWR`, `lseek`, symlink.
- Gli fd vivono nel server, non nel client: chiudere/duplicare/ereditarli
  segue le regole dei canali (ADR-0008/0010), non quelle di `fork`.
- Permessi Strato 0: campi `mode`/`opts` placeholder, zero enforcement
  (ADR-0013).

## Confronto microkernel

Linux (VFS nel kernel, POSIX = ABI stabile), MINIX3/QNX (filesystem server in
userspace con API POSIX sopra — il nostro modello), Fuchsia (FIDL: interfaccia
versionata esplicita sopra protocollo interno libero — il principio qui
adottato, senza IDL).

## References

- ADR-0005 (microkernel: kernel fuori dal FS), ADR-0013 (permessi a strati),
  ADR-0014 (diritti per-canale, estensione non-POSIX)
- `docs/src/09-filesystem.md`, `libs/libr/src/lib.rs`
