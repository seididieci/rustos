# ADR-0016: FAT32 scrivibile (persistenza)

Data: 2026-09-15
Stato: accettato (Fase 20)

## Contesto

Dalla Fase 9.2 il FAT32 e' read-only: `handle_write_local` rifiutava
`FsKind::Fat`, il protocollo `DISK_*` aveva solo READ e `fat32.rs` era un
parser puro. La persistenza scrivibile esisteva solo in ramfs (persa al
reboot). La shell documentava onestamente "niente write su /fat" (Fase 18.3).

## Decisione

FAT scrivibile in userspace, **write-through senza cache**, a sotto-fasi
indipendentemente validabili:

- **20.0** `DISK_WRITE` (0x55): un settore per chiamata, frame `[512:8][settore]`
  nel DISK_REQ ring (handle in w0, lba in w1); reply senza frame. Stesso
  contratto dei ring FS (frame consumato sempre, anche a handle/lba invalidi).
- **20.1** `write_sector` PIO (`WRITE SECTORS (EXT)` 0x30/0x34 + `FLUSH CACHE`
  0xE7/0xEA) con lo stesso polling bound dei read; `BlockSource::write_sector`.
- **20.2** Overwrite entro `size` (read-modify-write a settori).
- **20.3** Crescita con allocazione cluster (scan free, link immediato, doppia
  copia FAT, azzeramento coda, size nella dir-entry PER ULTIMA), FSInfo
  aggiornato se presente (altrimenti skip silente).
- **20.4** `O_CREAT` su /fat (entry 8.3 maiuscola, no LFN, attr archivio).
  `mkdir`/`rmdir`/`rm` su FAT restano fuori scope (niente unlink).

Ordine crash-safe (link → zero → dati → size): un crash lascia cluster
allocati oltre-size (leak, fsck-fixabile) ma mai spazzatura leggibile.

## Conseguenze

- `/fat` non e' piu' readonly: `STAT_READONLY` rimosso per FAT (resta per il
  bit in protocollo, usato da nessun nodo); `cp` verso /fat crea/scrive,
  `rm` resta rifiutato (manca l'unlink, non il permesso).
- `test-shell.py` ribaltato sui check cp/rm (write+read-back veri).
- Validazione: `testfat` 7/7 (overwrite+restore pristino, create+grow 9000 B
  multicluster con pattern), `fsck.fat -n` pulito dopo sessione completa
  (6 file, 9 cluster), `mdir`/`mcopy` coerenti.

## Alternative scartate

- **Write-back caching**: coerenza gratis col write-through; PIO basta per i
  volumi di test. Rivalutare con DMA/IRQ.
- **Shrink/truncate**: richiede free dei cluster; mai servito dai test
  (il restore e' overwrite a pari size). Rimandato.
- **LFN**: le entry 0x0F restano saltate come in lettura. Rimandato.
