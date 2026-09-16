# Process Scheduler

> **Implementazione attuale**: scheduler unico **RT a 32 priorita' + CBS**
> (vedi [RT Scheduler + CBS](./10-scheduler-rt-cbs.md), Fase 11). Questo
> capitolo introduce i concetti (PCB, context switch, stati, scheduling); i
> dettagli della run queue O(1) e del Constant Bandwidth Server sono nel
> capitolo dedicato.

## Panoramica

Lo scheduler gestisce l'esecuzione concorrente di piu' processi:

1. **Process Control Block (PCB)**: struttura per ogni processo
2. **Context Switch**: salvataggio/ripristino dei registri CPU
3. **Preemptive scheduling**: il timer PIT forza i context switch
4. **Priorita' + bandwidth reservation**: i processi critici ricevono una quota
   garantita di CPU anche sotto carico al 100%

## Stato attuale

Velordor usa un **unico scheduler RT**: 32 livelli di priorita' (0 = idle,
31 = massima) con run queue per-priorita' O(1), piu' Constant Bandwidth Server
(CBS) per i task che richiedono continuita' garantita. Dettagli in
[RT Scheduler + CBS](./10-scheduler-rt-cbs.md).

A livello di meccanismo:

- IRQ timer (PIT, 100 Hz) richiama `sched::on_tick()`, che forza lo switch a
  fine quantum (2 tick = 20 ms) e contabilizza il budget CBS
- IRQ keyboard (Fase 15): il kernel fa solo routing + EOI e sveglia il driver
  userspace `userkbd` via `IRQ_NOTIFY_KBD` (messaggio in coda — senza, un
  driver in `recv()` a coda vuota si ri-bloccherebbe senza mai leggere
  l'hardware); `userkbd` (ring 3, porte 0x60-0x64) drena l'i8042 e pubblica
  gli scancode su `/dev/kbd`, `usertty` li decodifica (v. ADR-0011)
- l'`idle process` (priorita' **Idle** = 0) esegue `hlt` quando nulla e' pronto
- scheduling **solo timer-driven**: nessuno switch volontario; la priorita'
  seleziona il prossimo processo, lo switch e' un meccanismo separato

### Context switch reale

Ogni processo ha il **proprio stack kernel** (frame fisici, fuori heap,
mai spostati). Il primo avvio usa un trampoline `naked` che esegue `iretq` su
un `InterruptStackFrame` fittizio preparato in cima allo stack (per i processi
user, su un frame ring 3 con CS/SS utente). Lo switch salva/ripristina i soli
registri callee-saved + RSP (`CpuContext`): i caller-saved restano sullo stack
di ciascun processo. Il `Mutex` dello scheduler viene rilasciato **prima**
dello switch (spin-lock non ricorsivo) e l'EOI viene inviato dal
`timer_handler` **prima** di `on_tick`, cosi' il PIC non resta bloccato
durante il passaggio.

La preemption e' verificata: un processo busy-loop viene sospeso forzatamente
dal timer e altri processi continuano a girare.

File: `context.rs` (`CpuContext`, `switch_to`, preparazione stack),
`process.rs` (PCB con stack dedicato e stati), `sched_rt.rs` (schedule,
pick_next, on_tick, block_current, wake — esposto come `crate::sched`),
`idle.rs`, `cbs.rs` (bandwidth reservation).

## Process Control Block (PCB)

Ogni processo ha una struttura PCB (`kernel/src/process.rs`). I campi
rilevanti per lo scheduling:

```rust
pub enum State {
    Ready,       // pronto per essere schedulato
    Blocked,     // in attesa di un evento (es. IPC, scancode)
    Terminated,  // finito, non piu' schedulabile
}

pub struct Process {
    pub id: usize,
    pub priority: crate::sched::Priority,  // u8 0-31, costante alias
    pub state: State,
    pub stack_top: u64,                    // stack kernel (RSP0 per il TSS)
    pub saved: CpuContext,                 // registri salvati al context switch
    pub kernel_stack_top: u64,
    pub tss_sel: SegmentSelector,          // TSS per-processo (ADR-0006)
    // ... campi IPC (msg_queue, reply_chan/req, ecc.) e CBS (cbs_server)
}
```

Lo stato IPC non vive nel PCB condiviso `PERCPU` ma nel PCB: ogni processo ha
la propria coda messaggi e i propri campi di reply, quindi resta valido
attraverso i context switch.

## Context Switch

Il context switch salva lo stato del processo corrente e ripristina quello del
prossimo:

```
Processo A in esecuzione
    │
    ▼ Timer interrupt
Salva i registri callee-saved di A nel CpuContext di A
    │
    ▼ Scegli il prossimo processo B (pick_next)
Carica il CpuContext di B
    │
    ▼ Prepara la CPU per B (TSS per-processo + CR3)
    │
    ▼ Riprende in B (context_switch)
```

Il salvataggio riguarda i soli registri **callee-saved** + RSP (`CpuContext`):
rax/rcx/rdx/rsi/rdi/r8-r11 (caller-saved) restano sullo stack del processo e
non vanno salvati. Vedi `kernel/src/context.rs`.

## Stati dei processi

```
            spawn
  ┌──────────┐   ───────────────►  ┌─────────┐
  │  (creato) │                     │  Ready  │
  └──────────┘                      └────┬────┘
                                         │
                              pick_next()│ (on_tick)
                                         ▼
                               ┌───────────────┐   block (IPC/wait)   ┌─────────┐
                               │  (in esecuzione)│ ───────────────────► │ Blocked │
                               └───────┬───────┘                       └────┬────┘
                                       │                                  │ wake
                                       │ quantum scaduto                   ▼
                                       │                              (Ready)
                                       ▼
                                   (Ready)
  exit ─────────────────────────────► Terminated
```

## Criteri di scheduling

| Criterio | Come funziona in Velordor |
|----------|--------------------------|
| Fixed-priority (32 livelli) | il processo a priorita' piu' alta tra i Ready gira per primo |
| Round-robin nel livello | a pari priorita', i processi si alternano a ogni quantum (2 tick) |
| Budget CBS | un task con server CBS riceve Q tick garantiti ogni P, anche sotto carico |
| Idle | quando nessun processo e' Ready, l'idle esegue `hlt` |

## Excursus storico — Fase 5

La Fase 5 introdusse il primo scheduler preemptive: 3 priorita'
(`High`/`Normal`/`Low`) con bitmask `u64` per livello e `pick_next` O(1). Con
la Fase 11 e' stato scritto da zero uno scheduler RT a 32 priorita' + CBS
(`sched_rt.rs`); dopo la validazione sulla suite completa lo scheduler a 3
priorita' e' stato **rimosso** e RT e' l'unico scheduler (ADR-0007). La
struttura del PCB e il meccanismo di context switch descritti sopra sono
invariati rispetto alla Fase 5.

## Lifecycle dei processi: cleanup kernel-side, kill e riuso dei PID (Fase 14)

Da ADR-0010, la morte di un processo (exit volontaria o `kill`) e' gestita in
**due tempi**:

 1. **Morte logica** (`Scheduler::terminate`, immediata, sotto lock): marca il
    processo `Terminated`, salva l'exit code, lo toglie dalla ready queue,
    rilascia CBS/servizi/canali che lo coinvolgevano, **risveglia i peer bloccati
    in `send` sincrono verso il morente** (campo `waiting_pid` → tornano con
    errore e possono rifare un `service_lookup`), enumera le coppie
    `(peer, channel)` da notificare (salvate nel PCB in `die_peers`, max 31
    peer distinti), termina la discendenza non-detached in cascata e
    ri-parenta a init i figli detached (flag spawn, Fase 22), e accoda il PID
    alla coda di reclaim. `init` (pid 1) non deve mai morire (panic
    documentato). Killabile: qualunque processo user tranne init, i processi
    kernel e se stesso.
 2. **Teardown fisico differito** (`Scheduler::drain_reclaim`, a inizio
    `on_tick`): libera i frame dello stack kernel, lo slot TSS (pool riusabile),
    l'address space user (walk delle page table dal CR3: foglie marcate `owned`
    + page-table frames; le pagine iniettate con `map_physical`/`map_in` — VGA,
    ring di altri processi — non si toccano) e azzera `HEAP_BRK`/`RING_PHYS`.

 Solo **dopo** il teardown il kernel notifica **tutti i peer** del morente
 (messaggio `EXIT_NOTIFY`, w0 = code, w1 = pid, sul canale che li collegava:
 notifica unificata, non solo al parent) e il PID torna nel free-set.
 Notificare dopo il reclaim evita che in un loop spawn/exit il pool si
 esaurisca (le risorse sono gia' libere quando i peer si svegliano) e che un
 peer acceda alla memoria del morto durante il teardown.

 Conseguenze: il limite dei 32 PID e' ora di **concorrenza**, non cumulativo dal
 boot (i PID vengono riusati); TSS, canali e server CBS sono pool riusabili;
  chi attende un servizio (es. init) puo' osservarne la caduta via `EXIT_NOTIFY`
  e riavviarlo (init-restart con respawn + attesa SVC_READY, backoff e hold:
  Fase 14.12, t27/t28/t32). Generazioni PID complete: rimandate (serve un
  cambio di protocollo). Riferimento: ADR-0010 (emendato Fase 22 per il detach).

## Riferimenti

- [RT Scheduler + CBS](./10-scheduler-rt-cbs.md) — dettagli Fase 11
- [Writing an OS in Rust - Testing](https://os.phil-opp.com/testing/)
- [OSDev Wiki - Process Scheduler](https://wiki.osdev.org/Process_Scheduler)
- [Linux Kernel - Scheduler](https://www.kernel.org/doc/html/latest/scheduler/)
