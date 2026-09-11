# ADR-0007: Scheduler RT a 32 priorita' + CBS (bandwidth reservation)

## Status

Accepted. **Aggiornato (post-Fase 13/14)**: lo scheduler RT e' stato
consolidato come **unico scheduler** di rustOS — lo scheduler classico a 3
priorita' (`sched.rs`) e' stato rimosso, insieme alla feature Cargo
`rt_scheduler` e ai relativi `#[cfg]`. Motivo: dopo la validazione su tutta
la suite (Fase 13/14, 21/21 su entrambi gli scheduler) il doppio percorso di
manutenzione (ogni fix IPC applicato due volte, due build, t18/t19 solo su
RT) non era piu' giustificato. Vedi sezione "Consolidamento".

## Context

Il requisito e' garantire CPU time **anche sotto carico al 100%**: es. un
processo che registra una traccia audio non deve perdere sample perche' altri
task saturano la CPU. La sola priorita' fissa non basta: un task ad alta
priorita' che non si blocca mai affama comunque tutto il resto. Serve una
forma di **bandwidth reservation** (stile Constant Bandwidth Server: Linux
SCHED_DEADLINE, RTEMS, Rialto).

Lo scheduler dell'epoca (`kernel/src/sched.rs`) usava 3 priorita'
(`High`/`Normal`/`Low`) con bitmask `u64` per livello e `pick_next` O(1). Le
prove sul campo (test suite, shell) lo rendevano stabile: non andava riscritto
a rischio, ma andava reso **sostituibile** senza toccare i chiamanti.

Alternative valutate:

- **Rate Monotonic (RMS)**: assegna priorita' statiche = 1/periodo, ottimale
  tra le statiche ma presuppone task periodici con deadline = periodo. I task
  del sistema (fs, console, shell) sono **event-driven/aperiodici**: RMS non
  si applica senza wrapper periodici artificiali.
- **EDF puro**: schedulabilita' al 100% ma priorita' dinamiche, piu'
  complesso da debuggare e con failure modes meno predicibili.
- **Fixed-priority a 32 livelli da solo**: utile come base (FreeRTOS) ma senza
  reservation non garantisce nulla sotto overload.
- **Riscrivere `sched.rs` in place**: rischio di regressione sulla suite senza
  modo di tornare indietro.

## Decision (originale, Fase 11)

Introdurre un **secondo scheduler** scritto da zero in un file separato
(`kernel/src/sched_rt.rs`), selezionato a **compile time** con un feature flag
Cargo `rt_scheduler` (default off = scheduler classico). Con il flag attivo,
`sched_rt` viene esposto come `crate::sched` (via `mod sched_rt as sched` in
`main.rs`) → **i chiamanti non cambiano** (main/syscall/user_binary/process
continuano a usare `crate::sched::*`).

Il nuovo scheduler combina:

1. **Fixed-priority preemptive a 32 livelli** (0 = idle, 31 = max) con run
   queue per-priorita' O(1): `ready_by_prio: [u32; 32]` + `ready_prio_mask:
   u32`, `pick_next` via `leading_zeros()` (stile FreeRTOS CLZ).
2. **Constant Bandwidth Server (CBS)** per i task che richiedono continuita'
   garantita: ogni server e' definito da `(budget Q, period P)` in tick
   (1 tick = 10 ms); il processo collegato riceve garantiti `Q` tick ogni `P`.
   - Budget contabilizzato a ogni `on_tick`; a budget 0 il task viene
     **throttled** (non piu' scelto) fino al replenishment.
   - Replenishment alla scadenza della `deadline`: budget = Q, deadline += P.
   - **Admission control**: un nuovo CBS e' accettato solo se
     `Σ(Qi/Pi) + Q/P ≤ CBS_BW_CAP` (~70%); il resto della CPU resta ai
     processi fixed-priority.

Il tipo `Priority` resta uniforme per entrambi gli scheduler: nel classic e'
un enum `High`/`Normal`/`Low`; nel RT e' un `u8` 0-31 con costanti alias
(`Priority::High`/`Normal`/`Low`) cosi' il codice sorgente compila identico in
entrambe le configurazioni.

### Dettagli

- File: `kernel/src/sched_rt.rs` (scheduler completo), `kernel/src/cbs.rs`
  (struttura `CbsServer` + pool `MAX_CBS_SERVERS`).
- Feature in `kernel/Cargo.toml`: `rt_scheduler = []`.
- Selezione in `main.rs`:
  `#[cfg(not(feature = "rt_scheduler"))] mod sched;` +
  `#[cfg(feature = "rt_scheduler")] mod sched_rt as sched;`.
- Campo CBS nel PCB sotto feature (`process.rs`): `cbs_server:
  Option<usize>`.
- Syscall nuove: 28 = `cbs_create`, 29 = `cbs_attach`, 30 = `cbs_get_info`.
- Mapping priorita' dei processi esistenti nel RT: idle=0, demo/uptime/
  testspin=1, test/demo=2-5, servizi Normal (console/fs/devfs/shell)=16-20,
  utspin_high/keyboard/urgenti=31; quantum invariato (2 tick).

### Alternative scartate

- **RMS**: inadatto a carico aperiodico/event-driven (vedi Context).
- **EDF puro**: complessita'/debug vs beneficio su un sistema con vincoli di
  latenza e debugabilita' richiesta.
- **Single scheduler riscritto in place**: nessun rollback possibile.
- **Parametro kernel a runtime per scegliere lo scheduler**: richiede
  dispatch dinamico (trait object o enum) che complica i percorsi hot
  (on_tick/pick_next); il compile-time flag e' piu' semplice e a costo zero.

## Consequences

- Il default (`./run.sh`, `cargo build --release`) continua a usare lo
  scheduler classico: suite invariata e verde. (Vale per la decisione
  originale; oggi lo scheduler e' uno solo, vedi Consolidamento sotto.)
- Con `--features rt_scheduler` si testa il nuovo scheduler sulla STESSA
  suite: validazione incrociata (testfs/testfat/usertests/test-shell).
- Un task CBS (es. audio Q=2, P=10 → 20%) completa sempre i suoi tick nel
  periodo anche con la CPU saturata da task fixed-priority.
- Il CBS tempo non usato (task bloccato) non si accumula: va ai
  fixed-priority → nessuno spreco di CPU.
- Costo: un secondo scheduler da mantenere (rischio duplicazione); mitigato
  dalla superficie pubblica identica e da `process.rs`/`cbs.rs` condivisi.
- Il campo `cbs_server` aumenta il PCB solo con la feature attiva.

## Consolidamento (post-Fase 13/14)

Dopo la validazione su tutta la suite (Fase 13/14: testfs 5/5, testfat 6/6,
usertests 21/21 e shell 3/3 con **entrambi** gli scheduler), la motivazione
del dual e' venuta meno:

- lo scheduler RT e' l'unico testato e mantenuto; il classico richiedeva di
  applicare due volte ogni modifica al percorso IPC (send/recv/reply, async),
  due build e due giri di regressione;
- i test CBS (t18/t19) nel classic passavano "vuotamente" (il CBS non esisteva
  li'); l'unico scheduler li rende sempre reali;
- il tipo `Priority` era gia' uniforme (costanti alias), quindi la rimozione
  del classic non tocca i chiamanti.

Decisione di consolidamento:

1. **Rimosso** `kernel/src/sched.rs` (scheduler classico 3 priorita').
2. **Rimossa** la feature Cargo `rt_scheduler` e tutti i `#[cfg(feature =
   "rt_scheduler")]` (cbs.rs, `cbs_server` nel PCB, syscall 28-30, selezione in
   main.rs).
3. `kernel/src/sched_rt.rs` mantiene il nome "rt" ma e' esposto in modo
   stabile come `crate::sched` (via `#[path = "sched_rt.rs"] mod sched;` in
   main.rs) ed e' l'**unico** scheduler, sempre attivo.
4. Il CBS e le syscall 28-30 sono sempre disponibili; `usertests` t18/t19 sono
   sempre reali (nessun ramo "vacuo").
5. `run.sh` non ha piu' il flag `--rt`: build e boot unici.

Documentazione: questo ADR (sezione di consolidamento), `05-scheduler.md`
riscritto su RT (con excursus storico Fase 5), `10-scheduler-rt-cbs.md`,
`11-testing.md`, `06-syscalls.md`, AGENTS.md.
