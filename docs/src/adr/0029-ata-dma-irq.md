# ADR-0029: ATA DMA + IRQ (Bus-Master PIIX, attesa event-driven, wakeup-preemption)

## Status

Accepted (implementata, Fase 38 — gate 5/5 + 7/7 + 52/52 + shell verde,
zero FAIL/PANIC/FAULT; tabelle in `13-performance.md` §38).

## Context

Dopo la Fase 25 il collo di bottiglia misurato resta il disco: ~1,2 ms per
settore in PIO, con `userdisk` bloccato nel polling porta-per-porta
(ADR-0012/0016 anticipano entrambe che "lì l'async avrà senso"). Il PIO brucia
inoltre ~1,2 ms di CPU Normal a transfer. Obiettivo 38: offload Bus-Master +
completamento a interrupt, senza cambiare il protocollo `DISK_*` (userfs
intoccato) e senza regressioni (soglia repo: ±10% sullo stesso host KVM).

## Decision

**Motore DMA in `userdisk`, tutto in userspace; kernel solo routing IRQ.**

- `SYS_DMA_ALLOC` (49): 1 pagina staging contigua (PRD a offset 0, dati a 64),
  mappata a `USER_DMA_VA`, free a teardown/exec, skip in fork come i ring.
- `libr::pci` (scan bus 0 → PIIX3-IDE 8086:7010, BAR4 a `BM_BASE` 0xC000,
  IO+BM nel command) + `io_ranges` (`0xCF8-0xCFF`, `0xC000-0xC00F`); IDENTIFY
  word 63/88 + SET FEATURES (min(drive,UDMA2)); fallback PIO a qualunque
  verifica fallita.
- Transfer: PRD (split confini 64K, EOT, cap 8) + `READ/WRITE DMA EXT` su
  staging + `FLUSH CACHE` per le write; qualunque `false` = fallback PIO
  per-op (stesso contratto). DEV relay resta PIO.
- IRQ14/15 → `disk_irq` (lookup owner `Disk`, notify `IRQ_NOTIFY_DISK`,
  EOI slave+master), PIC slave smascherato bit 6-7 **e cascade IRQ2 sul
  master** (38.0e: era mascherato dalla Fase 3, slave sordo — `pic0 irr=04
  imr=fc`, 317 assertion mai vettorate).
- Attesa **event-driven**: `start_dma` arma, il server dorme in `recv`
  (fast-path pre-check, EXIT-abort attribuito via `peer_pid`), `finish_dma`
  chiude. CPU libera per ~device-time a transfer invece del poll.
- **Wakeup-preemption centrale** in `notify_irq`: dopo EOI, se
  `select_next() == svegliato`, switch diretto (stesso pattern di `on_tick`).
  Senza, ogni wakeup paga ~1 tick (10 ms) — misurato 140x su fat_small
  (vedi sotto). EOI prima della notify nei due handler, come il timer.
- **Guardie reply in `pop_msg`**: i messaggi che non attendono mai reply non
  toccano la reply implicita — notify kernel (canale 0) ed EXIT_NOTIFY (peer
  morto: rispondere è impossibile per disegno, nessun server lo fa,
  verificato). Senza, un `recv` tra richiesta e reply perde la reply a userfs.

## Scoperte (ognuna misurata o osservata, mai teorizzata)

1. **Cascade mascherato** (38.0e): un bit (`0xFC→0xF8` in `pic.rs`); il
   commento "IRQ2 resta aperta" era falso dal boot.
2. **Reply clobberata** (38.1c, due hang diagnosticati): `pop_msg` riscriveva
   `reply_chan` a ogni `recv` con `req_id >= 0` (le notify hanno 0); la `send`
   sincrona scarta a coda piena ma blocca comunque → da qui poll + drain
   a testa-loop (resta load-bearing contro i re-fire level-triggered).
3. **Tick-wait** (38.2): l'event-driven senza preemption pagava ~1 tick/op
   (firme: `cyc_op` identici tra run = multipli di tick): fat_small 1.3M→180M
   cyc, fat_4K_oow 20M→810M. La sda sembrava immune ma il relay DEV è PIO
   senza wait (prova vacua, corretta onestamente). Con preemption: parità.
4. **EXIT-clobber** (38.2e, wedge permanente in `test-shell.py`): EXIT altrui
   in `wait_dma` (morte usertests a fine suite durante shell-load) → reply
   persa → userfs↔userdisk fermi. In 38.1c non esisteva (mai recv mid-op).
   Fix alla radice nel kernel: da userland la reply non si può ri-armare
   (niente `reply_to`).

## Consequences

- Latenza: parità entro la banda ±10% (device-bound; tabella §38).
- Meccanismo vivo a scala: `ev_wait`≈transfer, `fb=0`, `abort=0` su 4000+.
- Latenza IRQ→processo sub-tick per TUTTI i driver (tasti 10 ms→µs gratis;
  base per audio CBS e server-run async di userdisk, parcheggiati).
- CPU non più bruciata in poll per costruzione (~device-time/op a prio
  Normal restituito al pool; su KVM con host-IO cached il device costa
  ~50 µs e la differenza è sotto il rumore — dichiarato, non gonfiato).
- Rischi residui (documentati in `dma.rs`): device che accetta e non alza mai
  INTR appenderebbe il `recv` (mai osservato; pre-flight + re-fire +
  EXIT-abort coprono il resto); PIO fallback sempre disponibile.

## Alternatives considered

- `reply_to` esplicita / serve-while-pending (N richieste in volo): richiede
  syscall nuova + ABI; il device è seriale comunque (un taskfile) — rimandato
  al server-run async di userdisk, quando l'overlap lo richiederà davvero.
- Ibrido poll-breve-poi-block: tiene il bound ma uccide quasi tutto il
  guadagno CPU (il tipico completa nel poll) — scartato.
- Restare a poll (38.1c): nessuna regressione ma nessun guadagno e nessun
  wakeup veloce per gli altri driver — scartato dopo le misure.
