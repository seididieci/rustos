# Scheduler RT a 32 priorita' + CBS (Fase 11)

> Fase 11 — **implementata e verificata** (vedi [ADR-0007](./adr/0007-rt-scheduler-cbs.md)).

## Motivazione

Lo scheduler della Fase 5 (storico, 3 priorita' `High`/`Normal`/`Low`, poi
rimosso a favore di questo RT — vedi sopra) garantiva solo un **ordine
relativo**: il processo ad alta priorita' gira prima di quello basso, ma se un
task non si blocca mai, la CPU gli appartiene per sempre. Non c'e' alcun
**limite massimo** al consumo di un processo ne' alcuna **quota minima
garantita** a un altro.

Serve quindi qualcosa di piu' forte: garantire che un task critico riceva
**sempre** una frazione di CPU, anche quando il resto del sistema satura il
100%. Il caso d'uso guida e' la registrazione audio:

```
CPU al 100% (shell + fs + test che girano senza sosta)
           ┌──────────────────────────────────────────┐
task audio │  deve comunque ricevere i SUOI campioni  │  ← nessun sample perso
           └──────────────────────────────────────────┘
```

Questo problema non si risolve con le priorita' ma con la **bandwidth
reservation** stile Constant Bandwidth Server (CBS) — la tecnica usata da
Linux `SCHED_DEADLINE`, RTEMS e Rialto (Microsoft Research).

## Architettura: lo scheduler unico

Lo scheduler RT e' l'**unico** scheduler di rustOS. E' nato nella Fase 11 come
secondo scheduler selezionato a compile time accanto a quello classico (3
priorita'); dopo la validazione su tutta la suite (Fase 13/14, 21/21) lo
scheduler classico e' stato rimosso e RT resta sempre attivo, senza feature
flag (vedi ADR-0007).

```
kernel/src/
├── sched_rt.rs     scheduler RT (32 priorita' + CBS)   ← l'unico scheduler
├── cbs.rs          struttura CbsServer + pool (sempre compilato)
└── process.rs      PCB con campo cbs_server (sempre presente)
```

In `main.rs` e' esposto come `crate::sched`:

```rust
#[path = "sched_rt.rs"]
mod sched;                      // l'unico scheduler, esposto come crate::sched
mod cbs;
```

Tutti i chiamanti (main, syscall, user_binary, process) usano
`crate::sched::*`:

`init` · `spawn` · `create_user` · `on_tick` · `block_current` · `wake` ·
`ipc_send` · `ipc_recv` · `ipc_reply` · `exit_current` · `process_of` ·
`process_name` · `process_cr3` · `IpcResult` · `Priority`

### Build

```bash
# Scheduler RT a 32 priorita' + CBS (unico, sempre attivo)
cargo build --release
```

## Priorita' a 32 livelli

Il tipo `Priority` e' un newtype `u8` 0-31 con costanti alias:

```rust
// sched_rt.rs — u8 0-31 con costanti alias
pub struct Priority(pub u8);
pub const High: Priority   = Priority(31);   // massima
pub const Normal: Priority = Priority(16);   // servizi interattivi
pub const Low: Priority    = Priority(1);    // demo/uptime/testspin
pub const Idle: Priority   = Priority(0);    // minimale
```

Convenzione FreeRTOS: **numero piu' alto = priorita' piu' alta**. Il codice
sorgente (es. `Priority::High` in `user_binary.rs`) usa le costanti alias.

### Mapping dei processi esistenti

| Processo | Priorita' RT |
|----------|--------------|
| `idle` | 0 |
| `useruptime` (solo informativo) | 1 |
| servizi e test (console, fs, devfs, shell, usertests, init, helper) | 16 |
| urgente (es. spin a 31 nei test di priorita' t17) | 31 |

Quantum invariato: **2 tick** (20 ms).

## Run queue per-priorita' (O(1))

Due strutture sostituiscono la vecchia `[u64; 3]`:

```rust
struct Scheduler {
    // ...
    /// ready_by_prio[p] : bit i = processo PID i pronto al livello p
    ready_by_prio: [u32; 32],
    /// ready_prio_mask : bit p = almeno un processo pronto al livello p
    ready_prio_mask: u32,
}
```

`set_ready` / `clear_ready` sono O(1):

```rust
fn set_ready(&mut self, pid: usize) {
    let p = self.processes[pid].priority as usize;
    self.ready_by_prio[p] |= 1u32 << pid;
    self.ready_prio_mask |= 1u32 << p;
}

fn clear_ready(&mut self, pid: usize) {
    let p = self.processes[pid].priority as usize;
    self.ready_by_prio[p] &= !(1u32 << pid);
    if self.ready_by_prio[p] == 0 {
        self.ready_prio_mask &= !(1u32 << p);
    }
}
```

`pick_next` trova il livello piu' alto con `leading_zeros()` (istruzione CLZ,
come FreeRTOS) e fa round-robin interno al livello sulla bitmask, ripartendo
dal bit successivo all'ultimo scelto A QUEL LIVELLO (`rr_cursor[p]`):

```rust
fn pick_next(&mut self) -> Option<usize> {
    if self.ready_prio_mask == 0 {
        return None;
    }
    // Livello di priorita' piu' alto con almeno un processo pronto.
    let p = 31 - self.ready_prio_mask.leading_zeros() as usize;
    let mask = self.ready_by_prio[p];
    let k = (self.rr_cursor[p] + 1) % 32;
    let rot = mask.rotate_right(k);
    let j = rot.trailing_zeros() as usize;
    let bit = (j + k as usize) % 32;
    self.rr_cursor[p] = bit as u32;
    Some(bit)
}
```

E' la generalizzazione a 32 livelli del bitmask di Fase 10 (10.1.3): nessuna
allocazione, nessuna scansione di liste.

> Lezione imparata (starvation deterministica): un contatore globale condiviso
> tra sottoinsiemi diversi NON e' un round-robin equo. Con cicli IPC
> deterministici il contatore si aggancia in fase con la sequenza dei
> sottoinsiemi e un membro non viene mai scelto: osservato sotto KVM (pid 7,
> userkbd, mai scelto in ~1900 pick tra {4,7}/{7,8}/{7,9} → tastiera muta dopo
> i primi tasti), mentre TCG — piu' lento — rompeva la fase con i pick dei
> quanti e mascherava il bug. Il cursore per-livello garantisce che ogni membro
> dell'insieme persistente venga scelto entro N pick, a qualunque velocita'.

## Constant Bandwidth Server (CBS)

### Idea

Un task critico dichiara due parametri:

| Parametro | Significato | Esempio audio |
|-----------|-------------|---------------|
| **budget Q** | ms di CPU garantiti per periodo | 2 tick (20 ms) |
| **period P** | intervallo in cui Q e' garantito | 10 tick (100 ms) |
| **bandwidth** | `Q / P` | 20% |

La promessa del CBS e':

> "Il task riceve SEMPRE almeno Q di CPU ogni P, perche' il resto del sistema
> non puo' consumarne di piu' di quanto gli spetti."

A differenza della priorita' fissa (che limita solo l'ordine), il CBS limita
anche il **consumo massimo** degli altri task.

### Struttura (`kernel/src/cbs.rs`)

```rust
pub const MAX_CBS_SERVERS: usize = 8;
pub const CBS_BW_CAP: f64 = 0.70;

pub struct CbsServer {
    pub budget_ticks: u32,       // Q: budget per periodo
    pub period_ticks: u32,       // P: periodo
    pub remaining_budget: i32,   // budget residuo nel periodo corrente
    pub deadline: u64,           // tick assoluto di fine periodo
    pub task_pid: Option<usize>, // processo servito
    pub active: bool,
    pub bandwidth: f64,          // Q / P (solo kernel: get_info NON la espone)
}
```

Pool statica: `static CBS_POOL: Mutex<[Option<CbsServer>; 8]>` — nessuna
heap allocation, slot fissi.

**API**:

| Funzione | Descrizione |
|----------|-------------|
| `create(Q, P)` | Admission control + alloca slot → id o `Err(())` |
| `attach(id, pid)` | Lega un server a un processo |
| `get_info(id)` | Budget/period/remaining (debug/test; niente bandwidth) |
| `tick_budget(pid) -> bool` | Decrementa budget; `true` se throttled |
| `tick_replenish() -> Replenished` | Replenish deadline scadute, ritorna struct stack (array PID + len, mai `Vec`) da rischedulare |

**Campi nel PCB** (`process.rs`):
`cbs_server: Option<usize>` — indice nel pool CBS, `None` = nessun server.

**Integrazione in `on_tick`** (ordine):

```
0. sched.drain_reclaim() → teardown processi Terminated (prima di tutto:
   mai liberare lo stack di chi gira, e i frame tornano riusabili subito)
1. cbs::tick_replenish()  → set_ready() per PID riapprovvigionati
2. logica quantum esistente (ticks_current >= QUANTUM_TICKS)
3. cbs::tick_budget(cur)   → clear_ready() se throttled, need_switch = true
4. pick_next() → switch_to() se necessario
```

### Ciclo di vita

1. **Creazione** (`cbs::create`): admission control → se accettato, il server
   entra nel pool con `remaining_budget = Q` e `deadline` al primo periodo.
2. **Contabilita'** (in `on_tick`): `cbs::tick_budget(pid)` decrementa
   `remaining_budget` del server associato al processo corrente.
3. **Throttle**: quando `remaining_budget == 0`, `tick_budget` ritorna `true`
   → `on_tick` chiama `clear_ready(cur)` e forza lo switch. Il task **non
   viene piu' scelto** finche' il budget non e' ripristinato.
4. **Replenishment**: `cbs::tick_replenish()` controlla la `deadline` di ogni
   server. Se scaduta: `remaining_budget = Q`, `deadline += P`, e il PID viene
   rimesso in ready queue (`set_ready`). Il processo torna schedulabile.

```
tick:       0  1  2  3  4  5  6  7  8  9  10  11  12 ...
            ┌──────┐                    ┌──────┐
audio (Q2P10)│  run │  throttled...      │  run │  ← SEMPRE 2 tick ogni 10
            └──────┘                    └──────┘
```

Il tempo CBS **non usato** (task bloccato in `recv`/`send`) NON si accumula:
finisce ai processi fixed-priority → nessuno spreco di CPU.

### Admission control

Quando si crea un server si verifica che la somma delle bandwidth non superi
il cap:

```
Σ(Qi/Pi) + Q/P ≤ CBS_BW_CAP        (CBS_BW_CAP ≈ 0.70)
```

Il 30% rimanente resta ai processi fixed-priority (e assorbe gli overrun).
Oltre il cap la richiesta viene **rifiutata** (syscall ritorna -1).

## Syscall CBS e wrappers libr (Fase 11.4)

| Numero | Syscall | Semantica |
|--------|---------|-----------|
| 28 | `cbs_create(budget, period)` | crea un server CBS → id o -1 (admission control) |
| 29 | `cbs_attach(server_id)` | lega il server al processo corrente |
| 30 | `cbs_get_info(server_id)` | budget/period/remaining del server (debug/test; niente bandwidth) |

Wrapper in `libr` (`libs/libr/src/lib.rs`):

```rust
pub fn cbs_create(budget_ticks: u32, period_ticks: u32) -> Result<i64, ()>;
pub fn cbs_attach(server_id: i64) -> Result<(), ()>;
pub fn cbs_get_info(server_id: i64) -> Option<CbsInfo>;   // budget/period/remaining (niente bw)
```

### Uso tipico (processo "audio")

```rust
match libr::cbs_create(2, 10) {           // Q=2, P=10 → 20% garantito
    Ok(id) => { libr::cbs_attach(id).ok()?; }
    Err(()) => return,                     // admission rifiutato
}
loop {
    // campiona l'hardware, riempi il buffer audio...
    // il kernel garantisce: mai piu' di P tick senza i Q tick spettanti.
}
```

## Verifica

**Stato attuale**: CBS implementato nel kernel (`cbs.rs` + integrazione
`on_tick`), syscall 28-30 e wrappers libr completi, test specifici CBS (t18
admission + t19 bandwidth) implementati e verdi. Lo scheduler RT e' l'unico
scheduler (il classico a 3 priorita' e' stato rimosso dopo la validazione) —
il CBS non interferisce con i processi che non lo usano.

### Test bandwidth (11.5.1) — PASS

Task "audio" (`utcbstest`) con CBS (Q=3, P=10 → 30%) + task hog (`testspin`
a prio Normal, senza CBS, budget 300 tick) che satura la CPU. L'audio deve completare
**sempre** i suoi ~3 tick ogni 10 (nessun sample perso).

Misura: l'audio e l'hog **contano ciascuno i tick PIT osservati** durante il
proprio busy-loop e li riportano al parent con `T_DONE` (w1). NOTA
implementativa: il busy-loop fa **batch da 512 spin puri** tra due `get_ticks`
(syscall) — un get_ticks per iterazione maschera IF=0 e affama il timer (il
wall-clock non avanza e il budget non scade). Il parent resta **bloccato in
`recv`** (mai spin su syscall): riceve i due DONE e valuta.

Risultati attesi/osservati: audio ~60/200 (30% → margine [40,90]), hog
~243/300 (~70%) → check `audio_obs in [40,90] && hog_obs > audio_obs` → PASS.

### Test admission control (11.5.2) — PASS

Una richiesta con `Σ bandwidth > cap` (~70%) deve essere rifiutata (-1):
80% singola e 75% cumulativa rifiutate; 5% + 10% accettate (15% totale).

### Validazione (11.5.3) — PASS

Gate dell'epoca (unico scheduler; il corrente e' in `11-testing.md`):

```
boot pulito + [testfs] PASS 5/5 + [testfat] PASS 7/7
             + [usertests] PASS 40/40 + test-shell.py ~30/30
```

### Fix CBS importanti emersi dalla validazione

1. **Rilascio del server CBS a `exit_current`**: quando un processo con CBS
   esce, il server resta `active` con `task_pid` che punta a un processo
   Terminated. `tick_replenish` lo risvegliava a ogni deadline (`set_ready` su
   un processo morto) e il scheduler lo riprendeva dentro il `loop { hlt() }`
   di `exit_current` con IF=0 → congelamento totale. Ora
   `cbs::release_pid(pid)` deattiva i server del processo uscente.
2. **Replenish solo su processi vivi**: `on_tick` ri-aggiunge in ready queue
   SOLO processi `Ready` (o throttle-ati), mai `Terminated` o `Blocked`
   (quest'ultimo lo sblocca la reply IPC, non il CBS).

## Perche' non Rate Monotonic / EDF

- **RMS** assegna priorita' = 1/periodo ed e' ottimale tra le statiche, ma
  presuppone task **periodici** con deadline = periodo. I servizi del sistema
  (fs, console, shell) sono event-driven/aperiodici: RMS richiederebbe wrapper
  periodici artificiali. Inoltre il bound di Liu & Layland garantisce solo
  ~69% di utilizzazione.
- **EDF puro** raggiunge il 100% ma con priorita' dinamiche: scheduling piu'
  complesso e failure modes meno predicibili per un sistema con vincoli di
  latenza e debugabilitá richiesta.
- Il **fixed-priority a 32 livelli + CBS** copre sia i task best-effort (gli
  altri, gia' schedulati per priorita') sia i task con continuita' garantita,
  ed e' la strada seguita dai sistemi reali citati.

## Riferimenti

- ADR: [0007](./adr/0007-rt-scheduler-cbs.md)
- Implementazione: `AGENTS.md` — Fase 11
- File: `kernel/src/sched_rt.rs`, `kernel/src/cbs.rs`, `kernel/src/process.rs`
- Strutture dati Fase 10: bitmask `pick_next` (10.1.3), coda IPC ring (10.1.2)
