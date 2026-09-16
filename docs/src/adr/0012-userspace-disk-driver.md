# ADR-0012: Disk driver ATA in userspace (userdisk)

**Status**: Implemented (Fase 16)
**Data**: 2026-09-14

## Contesto

Fino alla Fase 15 `userfs` possedeva tutto lo storage: driver ATA PIO
(`block.rs`, `io.rs`), parser FAT32 e porte `0x1F0-0x1F7`/`0x3F6-0x3F7` abilitate
solo per lui (TSS per-processo, ADR-0006). Ogni read `/fat` bloccava il server
nel polling PIO e il disco era hardcoded (primary master, LBA28, FAT dal
settore 0 senza enumerazione ne' partizioni), contro il disegno microkernel
(ADR-0005: driver in userspace).

## Decisione

Split in due, come Fase 15 (kbd/tty):

- **`userdisk`** (driver ATA puro): possiede entrambi i canali
  (`0x1F0/0x3F6` + `0x170/0x376`) via `io_ranges`, rileva i dischi via
  `IDENTIFY 0xEC` (reset SRST, probe master/slave, ATAPI skippato con log,
  LBA48 via `READ SECTORS EXT`), parsa le partizioni MBR primarie ed espone
  ogni nodo come `/dev/sdX` (`FS_REGISTER` per nodo, solo i presenti) +
  servizio `Disk` per il data-plane. Bloccato in `recv`, mai `send` sincrone
  verso userfs (vedi regole).
- **`userfs`**: perde `io.rs`/`block.rs` e le porte ATA (`io_ranges` vuoto:
  qualunque `in/out` e' `#GP`, la suite verde lo certifica); il parser FAT32
  e' generico su trait `BlockSource`, implementato da `IpcDisk` (client
  `DISK_*` con riconnessione lazy). Mount con binding `/dev/sda→/fat` a boot
  (handle 0), fallback `ramfs-only` invariato; open raw `/dev/sdX` via parse
  del nome Linux (rel vuota), relay `DEV_*` generico invariato.

Init spawna `userdisk` prima di `userfs` (READY entrambi, come console:
l'ACK non aspetta il mount) e supervisiona anche `Disk`.

## Protocollo `DISK_*` (canale diretto, tag `0x50-0x53`)

`HELLO` (fisici ring nella reply: w0 = req riservato, w1 = resp mappato da
userfs), `OPEN`/`CLOSE` (validazione handle, reply OK/ERR senza frame),
`READ` (w0 = handle codificato `disco<<16|sub`, w1 = lba; un settore per
chiamata, 1:1 con `BlockSource`). Solo `READ` ha frame (`[512:8][0:8][settore]`
nel ring DISK dedicato a `0x24/0x25`, mai interleaving col traffico FS).
Handle dal nome, validita' nel driver (quante partizioni ha davvero il disco).

## Addendum Fase 16c (2026-09-14): resolve nome→handle lato driver

La mappa nome→handle era duplicata: `disk_handle()` in userfs indovinava
l'handle dal nome e `locate()` in userdisk lo validava. Ogni nuovo bus avrebbe
richiesto un parser nuovo in userfs. Ora userdisk e' la single source of
truth:

- Nuovo `DISK_RESOLVE` (`0x54`, tag centralizzati in `syscall-numbers`,
  riesportati da `libr`): richiesta frame `[namelen:8][name]` nel DISK_REQ
  ring (userfs mappa anche il req-phys da `HELLO.w0` a `0x24`, prima riservato
  e inutilizzato), reply w0 = handle o ERR. Stesse invarianti SPSC degli altri
  ring (consumer legge a `tail`, resync d'epoca a frame malformato).
- La tabella `nodes` alloca gli handle (disco<<16|sub); il relay raw `DEV_*`
  resta a handle (raw path intoccato).
- Decisione multi-bus bloccata (opzione d): userdisk resta l'unico owner del
  servizio `Disk` — SATA/NVMe futuri entrano come backend interni dietro un
  trait, nessun cambio kernel ne' emendamento ADR-0008 (registro a slot
  singolo: due driver separati non potrebbero coesistere sullo slot 7).

## Addendum Fase 16d (2026-09-14): identità stabile (UUID/LABEL) + listing

Le lettere `sdX` sono instabili (ordine di probe; un reorder le scambia): per
un mount persistente servono chiavi stabili. Si usano il seriale del volume FAT
(`vol_id`, 4 byte) e la label.

- `detect.rs` decodifica il seriale ATA (IDENTIFY word 10-19) in
  `DiskInfo.serial`; `fat32.rs` espone `vol_serial()`/`vol_label_trimmed()`;
  l'helper condiviso `libr::fat_bpb_identity` gestisce il layout standard
  (firma `0x29`@66) e quello legacy `mkfat` (firma@67).
- userdisk: `Node { name, handle, vol_uuid, vol_label }` + `sniff_identity()`
  (legge il BPB col proprio driver) + registrazione dei prefix
  `/dev/disk/by-uuid/<HEX8>` e `/dev/disk/by-label/<NOME>`. `DISK_RESOLVE`
  risolve nome → UUID → label (`resolve_node`).
- `scripts/mkfat.py` parametrizzato (`--serial`/`--label`/`--marker`) ed
  emesso nel layout BPB standard: il vecchio layout firma@67 faceva
  sovrapporre `label[0]` al 4° byte del seriale.
- Test: t36 (mount per UUID/LABEL + open raw by-path + listing) e
  `scripts/test-uuid-reorder.py` (due boot, ordine normale e `SWAP_DRIVES=1`:
  le lettere cambiano, UUID=/LABEL= no).

## Addendum Fase 16d (2026-09-14): register multi-prefix atomico (deadlock)

`devfs` registrava `/dev/null` e `/dev/zero` con DUE `fs_register` sincroni
consecutivi. Il primo crea un mount forwardable: se userfs, single-threaded,
in quel momento sta inoltrando una richiesta al driver (`send` bloccante),
il secondo register del driver si incrocia col forward di userfs → stallo
(riprodotto da t30 sotto flood: tutti i Normal bloccati, solo idle+uptime
runnable). Fix: `libr::fs_register_multi` (payload NUL-separato, UNA IPC) +
handler userfs che splitta; nessuna finestra in cui un mount esiste mentre il
driver e' ancora bloccato in un altro register. Regola: un driver che puo'
ricevere forward deve completare la registrazione in un'unica chiamata.

## Regole emerse (vincolanti)

1. **Mai sync incrociate tra server.** `userdisk` fa `FS_BUF_REG` + `R_REGISTER`
   solo via `send_async` con collect per req_id (SM come tty): una `send`
   sincrona verso userfs mentre userfs e' in `HELLO` sincrona verso userdisk
   e' deadlock certo (osservato a boot, entrambi bloccati fuori da `recv`).
   Vale anche per l'handshake `fs_init` di libr (sincrono): vietato ai driver.
2. **Mai throttle senza waker.** Ritenti falliti solo su `lookup`/`send` cheap
   e `recv` che blocca dopo: uno sleep con backoff in `recv` senza sorgente di
   wakeup congela i retry per sempre (osservato: registrazione ferma 8 s dopo
   un lookup fallito a boot; tty non soffre perche' ha `KBD_NOTIFY`).
3. **Consumer SPSC legge a `tail`.** Leggere l'header a `head` (appena avanzata
   dal producer) legge spazzatura oltre il frame (osservato: mount ok, prima
   read fallita, auto-guarigione via resync che mascherava tutto).
4. **`ring_alloc` = coppie fresche.** La cache single-pair restituiva le stesse
   pagine a ogni chiamata (cross-talk totale FS/DISK: +16/+528 fantasma negli
   head/tail). Ora ogni chiamata alloca nuovo + record multi-coppia con free a
   teardown (mapping syscall non-owned, mai double-free col walk `owned`).
5. **Partizioni nel driver, come strato.** La tabella MBR e' metadato del disco
   (`part.rs`, solo primarie, graceful se assente): niente processo separato
   (stesso trust/failure domain, solo hop IPC e slot in piu'). Nodi figli con
   prefix propri (`/dev/sda1`).

## Conseguenze

- `Service::Disk = 7`, `SERVICE_COUNT` 7→8 (registry scala da solo).
- Test t32 (raw `/dev/sda` + kill/restart + smoke `/fat` via riconnessione);
  `testfat` 6/6 invariata; suite 31/31 → 32/32.
- Verificato multi-disco (secondo `-drive if=ide`: `sdb` + `sdb1` da MBR).
- Limiti noti (fasi future): ATAPI/ISO9660, catene extended, caching,
  DMA+IRQ (li' l'async userfs avra' senso: col PIO polling nessuno overlap
  esiste). Realizzati dopo: mount espliciti in userspace (16b, ADR-0013) e
  scritture disco (Fase 20, ADR-0016: `DISK_WRITE` 0x55 + write PIO).
