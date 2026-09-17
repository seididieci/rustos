# ADR-0005: Architettura Microkernel

## Status

Accepted

## Context

Vincolo di progetto introdotto dopo la Fase 2: **Velordor deve essere un
microkernel**. Il piano originario era implicitamente monolitico (driver VGA e
seriale scritti dal kernel, user mode alla Fase 6 come extra, IPC assente).

In un microkernel il kernel contiene *solo*:

- scheduling
- IPC (message passing)
- gestione indirizzi / frame fisici
- routing interrupt ed eccezioni

Tutto il resto — driver, file system, shell — vive in **processi userspace**
che comunicano esclusivamente via IPC. Riferimenti reali: seL4, MINIX 3,
QNX, Fuchsia/Zircon, Redox OS.

## Decision

Tre sotto-decisioni di design:

### 1. IPC sincrona registro-based (stile seL4/L4)

Primitive `send(dest)` / `recv()` con messaggi passati nei registri CPU.
Motivazione: minimizza il codice e le code nel kernel mantenendo un contratto
IPC esplicito e a costo prevedibile; il modello resta evolvibile verso
porte/capabilities senza stravolgere le basi.

### 2. Address space: identity map + bit U/S

> **Nota di superamento (Fase HH, ADR-0020)**: kernel oggi higher-half
> (`-2G+1M`) + direct map, `PML4[0] = 0` a runtime. Resta il principio U/S;
> il resto del paragrafo e' storia della decisione originale.

Kernel e processi condividono lo stesso range lineare; la protezione è data
dal bit **U/S** delle PTE (pagine kernel = supervisor-only). Niente kernel
higher-half per ora. Motivazione: zero rilocazioni del codice esistente,
semplicità massima; invertibile in futuro senza stravolgimenti.

### 3. Debug console pragmatica

`vga.rs` e `serial.rs` restano nel kernel come **debug facility documentata**,
finché non esiste l'IPC (Fase 8): a quel punto driver VGA/keyboard migrano a un
*console server* userspace e il keyboard IRQ inoltra gli scancode via IPC.
Anche i microkernel di produzione hanno console di debug pre-IPC.

## Consequences

### Positive

- Isolamento dei guasti: il crash di un server non abbatte il sistema (restart)
- User mode e IPC diventano prerequisiti strutturali di ogni servizio: i driver
  e il file system sono costretti a un confine di privilegio esplicito
- Architettura allineata ai microkernel di produzione (seL4, MINIX, QNX,
  Fuchsia): scelte trasferibili e confrontabili con i sistemi reali

### Negative

- Ogni servizio attraversa il confine kernel/utente: overhead di context switch
- Roadmap cresce da 8 a 10 fasi; user mode anticipato da Fase 6 a bloccante
- I protocolli dei server (console, fs) entrano a pieno titolo nel design

### Neutral

- `vga.rs`/`serial.rs` marcati come debug facility temporanea
- Il keyboard IRQ resta gestito dal kernel ma *inoltra* gli scancode via IPC
- Il codice attuale (Fase 1-2) è già compatibile: eccezioni/GDT sono
  funzione nucleare legittima anche nei microkernel

## Roadmap rivista

| # | Fase |
|---|------|
| 1 | ✅ Boot PVH + long mode |
| 2 | ✅ Memory map + GDT/TSS/IDT |
| 3 | Interrupt hardware: PIC remap, PIT, IRQ tastiera (routing kernel-side) |
| 4 | Frame allocator fisico + heap kernel |
| 5 | Processi + scheduler preemptive (tick PIT) |
| 6 | **User mode** (ring 3) + entry syscall |
| 7 | **IPC sincrona** send/recv ⭐ |
| 8 | Primi processi utenti: init + **console server** (driver migrano qui) |
| 9 | File system server + protocollo IPC (ramfs poi FAT32) |
| 10 | Shell + utility |

> Nota storica: la tabella riflette la roadmap al momento dell'ADR (Fase 2).
> La numerazione effettiva delle fasi e' poi cresciuta e stata riordinata fino
> alla Fase 22 (IPC per nome = 12, IPC async = 13, cleanup = 14, tastiera/tty =
> 15, disk driver = 16, diritti = 17, shell = 18, introspezione = 19, FAT
> scrivibile = 20, servizi da disco = 21, detach = 22): vedi l'elenco corrente
> in `AGENTS.md` e la [tabella in 00-introduzione](./00-introduzione.md). Le
> decisioni piu' recenti sono negli ADR [0012](../adr/0012-userspace-disk-driver.md),
> [0013](../adr/0013-mount-syscall.md), [0014](../adr/0014-channel-rights-serverside.md),
> [0016](../adr/0016-fat-writable.md), [0017](../adr/0017-servizi-da-disco.md).

## References

- [seL4 - Manual](https://sel4.systems/Info/Docs/seL4-manual-latest.pdf) — IPC registro-based
- [MINIX 3](https://www.minix3.org/docs/) — driver in userspace con restart
- [Fuchsia Zircon concepts](https://fuchsia.dev/fuchsia-src/concepts/kernel/concepts)
- [Redox OS](https://doc.redox-os.org/book/) — microkernel in Rust
