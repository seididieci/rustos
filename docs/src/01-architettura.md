# Panoramica Architettura

## Architettura microkernel

Velordor è un **microkernel** (ADR-0005): il kernel contiene solo scheduling,
IPC, gestione della memoria e routing degli interrupt. Tutti i servizi —
driver inclusi — sono processi userspace che comunicano via IPC sincrona.

```
┌──────────────────────────────────────────────────┐
│                  USERSPACE (Ring 3)              │
│                                                  │
│  ┌───────┐   ┌─────────────────┐  ┌───────────┐  │
│  │ init  │◄─►│ console server  │◄─►│ fs server │  │
│  └───┬───┘   │ (VGA + kbd drv) │  │ (ramfs/   │  │
│      │       └────────┬────────┘  │  FAT32)   │  │
│      │                │           └─────┬─────┘  │
│  ┌───┴────────────────┴─────────────────┴─────┐  │
│  │            shell · ls · cat · echo         │  │
│  └────────────────────┬───────────────────────┘  │
└───────────────────────┼──────────────────────────┘
                        │ IPC sincrona (send/recv)
                        │ + syscall d'ingresso
┌───────────────────────▼──────────────────────────┐
│                  KERNEL (Ring 0)                 │
│                                                  │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐  │
│  │Scheduler │ │    IPC   │ │ Address spaces   │  │
│  │(RR su PIT│ │send/recv │ │ frame alloc +    │  │
│  └────┬─────┘ └────┬─────┘ │ paging per-proc  │  │
│       │            │       └──────────────────┘  │
│  ┌────┴────────────┴──────────────┐               │
│  │ Routing interrupt/eccezioni    │               │
│  │ IDT · PIC · PIT · IRQ tastiera │               │
│  └────────────────────────────────┘               │
├──────────────────────────────────────────────────┤
│              Hardware (QEMU x86_64)              │
└──────────────────────────────────────────────────┘
```

**Stato attuale** (Fase 8.x completata): la seriale e' l'unica debug console
in-kernel; il console server userspace gestisce VGA + decodifica tastiera
tramite IPC-inject dal kernel.

## Scelte progettuali del kernel

Le scelte originali introdotte in Velordor (sintesi in
[00-introduzione](./00-introduzione.md)) declinate a livello di kernel:

### Scheduler unico RT + CBS

Un solo percorso di scheduling a **32 priorita'** (0 = idle, 31 = max) con
run queue per-priorita' O(1) e **Constant Bandwidth Server**: un task con
riserva `(Q, P)` riceve Q tick garantiti ogni P anche sotto carico al 100%.
Niente dual scheduler classico/RT da mantenere: un'unica implementazione,
sempre attiva, esposta come `crate::sched`. Conseguenze: priorita' mappate su
`Priority(u8)` con costanti alias; admission control CBS (~70% della CPU);
quantum fisso (2 tick).

### IPC per nome kernel-side

Registry di servizi + oggetto `Channel` nel kernel: i peer si indirizzano per
nome di servizio, non per PID. `spawn` crea il **canale di nascita** (il
figlio lo usa come canale 0 = parent). La `reply` e' **implicita** al
messaggio correntemente elaborato (`reply_chan` impostato da `recv`): un
server single-threaded non deve tracciare richieste. Conseguenze: la morte di
un endpoint invalida i canali e libera lo slot servizio; riavvio/riuso sicuri.

### IPC async con request-id interno

`send_async`/`recv_nonblock` senza toccare l'ABI dei registri: il `req_id` e'
un **campo interno** di `PendingMsg` (signed: `>= 0` richiesta, `< 0`
risposta). La reply del server resta implicita: il kernel decide dallo stato
del target (bloccato → `reply_slot`; non bloccato → accoda con `req_id`
negativo). Conseguenze: backpressure esplicita (coda piena → errore), risposte
FIFO nel primo passo, server esistenti trasparenti all'async.

### Ring SPSC per-processo (zero-copy FS)

Ogni processo alloca due pagine ring (request + response) registrate presso il
file system server. I dati viaggiano direttamente tra i ring del client e il
server/driver, senza shared buffer ne' copie nel kernel. Conseguenze:
operazione FS = 1 IPC round-trip; il kernel non e' nel percorso dati; frame
grandi spezzati in piu' round-trip dal client.

### TSS per-processo con I/O bitmap

Ogni processo ha il proprio TSS con RSP0 e **I/O bitmap** (ADR-0006): i driver
userspace dichiarano in `io_ranges` le porte consentite a ring 3 (es. userfs →
porte ATA) e non possono toccare altre porte. Conseguenze: driver I/O reali in
userspace con minimo privilegio.

### Boot PVH custom

ELF64 + nota `XEN_ELFNOTE_PHYS32_ENTRY`, caricato da QEMU con `-kernel`;
`boot.asm` fa da trampolino PM32 → long mode. Conseguenze: zero dipendenze di
boot, percorso d'avvio interamente sotto controllo.

## Componenti del kernel

### 1. Boot (PVH)
- Loader QEMU carica l'ELF64 con nota `XEN_ELFNOTE_PHYS32_ENTRY`
- Entry in PM 32-bit flat @0x100000 → stub asm → long mode → `rust_main`

### 2. Boot info (`boot_info.rs`)
- Parsing `hvm_start_info` (layout Xen esatto, v1)
- Memory map e820-style: base per il frame allocator (Fase 4)

### 3. GDT/TSS (`gdt.rs`)
- Code64/data/TSS; stack IST dedicato al double fault

### 4. Interrupts (`interrupts.rs`)
- IDT + handler eccezioni sincrone (BP/PF/GPF/DF)
- Fase 3: PIC remappato, PIT, IRQ tastiera

### 5. IPC (Fase 7) ⭐
- Primitiva centrale del microkernel: send/recv registro-based stile seL4

## Protezione memoria

Strategia identity map + bit U/S (ADR-0005 §2):

```
Lineare == Fisico per tutto lo spazio basso.
Pagine kernel: PTE.U/S = 0 (supervisor) → ring 3 riceve #PF se le tocca.
Ogni processo avrà la propria PML4 (Fase 5+).
```

## Flusso tipico di una syscall nel progetto maturo

```
shell (ring3): write(fd, buf, n)
  → SYSCALL instruction → kernel: salva stato, valida argomenti
  → kernel traduce in messaggio IPC verso fs server (ring3!)
  → scheduler cede CPU al server → server risponde via IPC
  → kernel ripristina shell → ritorno da syscall
```

Tre cambi di contesto per una write: e' il costo strutturale del modello
microkernel (isolamento in cambio di round-trip).
