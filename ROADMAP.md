# ROADMAP Velordor

Sorgente unica di **stato** (fasi completate) e **futuro** (Pianificate +
Parcheggiate). La storia dettagliata per fase (decisioni, bug trovati,
lezioni, validazioni) vive in `docs/src/14-cronologia-fasi.md`; i gate
citati li' sono snapshot storici — il gate corrente e' in
`docs/src/11-testing.md`.

Orizzonte (dichiarato, non roadmap): self-hosting — un Velordor capace di
ricompilare se stesso. Ordina le priorita' (storage veloce prima, servizi
fuori dal kernel poi, personalita' per software reale dopo) senza pianificare
nulla da solo.

## Stato (completate 1-49)

Gate: `[testfs] PASS 5/5` + `[testfat] PASS 7/7` + `[usertests] PASS 57/57` +
shell, zero FAIL/PANIC/FAULT.

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
| 32 | Shared text ELF (segmenti immutabili condivisi, ADR-0022) | ✅ Completata |
| 33 | Infrastruttura COW (frame refcount + COW fault, ADR-0023) | ✅ Completata |
| 34 | `fork` — COW dell'address space (ADR-0024) | ✅ Completata |
| 35 | Hardening (threat model + cancelli kernel, ADR-0025/0026) | ✅ Completata |
| 36 | Identità misurata (hash nel PCB + manifest + policy su identità, ADR-0027) | ✅ Completata |
| 37 | `exec` in-place + shell che lancia programmi (run/jobs/wait, ADR-0028) | ✅ Completata |
| 38 | ATA DMA + IRQ (ADR-0029) | ✅ Completata |
| 39 | Fondamenta posix: registry 8→16 + `Service::Posix`, `libr::posix`, harness t53 (ADR-0030) | ✅ Completata |
| 40 | fd virtuali + redirect file (`> >> < 2> 2>&1`, `R_LSEEK`, `O_TRUNC`/`O_APPEND`, `R_DUP_*` modello B, errori tipati, t54) | ✅ Completata |
| 41 | Parser shell (quoting, `$VAR`, `; && \|\|`, glob) | ✅ Completata |
| 42 | Pipe + waitpid | ✅ Completata |
| 43 | Env/PATH/script (shebang, history; 43a ADR-0033 + 43b ADR-0034) | ✅ Completata |
| 44 | Job control + segnali (44a suspend/resume + fg/bg/Ctrl-Z, ADR-0035; 44b Ctrl-C selettivo + catch, ADR-0036) | ✅ Completata |
| 45 | Indurimento + chiusura posix (policy su identità + sandbox build, ADR-0037) | ✅ Completata |
| 46 | Provider trait filesystem (`LocalFs` + `LocalFsDyn`, `MountedFs::Local`, ADR-0038) | ✅ Completata |
| 47 | Wiring trait per ramfs (U1: open/read/write/readdir/stat/mkdir/delete) | ✅ Completata |
| 48 | Wiring trait per FAT32 (U2: `LocalFsDyn`, fix stat readonly) | ✅ Completata |
| 49 | Terreno pre-ArcaFS (T0: `AnyHandle`, mount-id stabili, `Source`+`fstype`, `Local` vivo, create/truncate nel trait) | ✅ Completata |

## Pianificate

Voci con scope e vittoria dichiarati (non date).

- [ ] **ArcaFS/object-store** (dopo T0/Fase 49): filesystem avanzato stile
      btrfs/zfs — snapshot, niente partizionamento, layout adattivo per tipo
      di device (hdd/ssd/emmc), raid avanzato, monitoring (SMART), COW su
      disco; hook futuro per distribuzione nativa (ceph/glusterfs-like).
      Prossimo passo: sessione guidata di analisi (modello dati, layout,
      raid, monitoring, COW, hook distribuito) → `arcafs.md` + ADR, poi
      implementazione. Vittoria: spec scritta + primo mount.

## Parcheggiate

Idee senza trigger (non date, solo su pressione reale).

- [ ] audio AC97+CBS (primo client servizio PCI, chiude ADR-0007 davvero)
- [ ] server-run async userdisk (quando l'overlap DMA lo richiede)
- [ ] Strato 3 credenziali
- [ ] ext2/ATAPI/write-back/read-ahead/`DISK_STATS`/generazioni PID/thread
      (solo su pressione reale)
- [ ] OOM-kill a load (negativa ADR-0028)
