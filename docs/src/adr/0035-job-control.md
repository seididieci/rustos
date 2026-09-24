# ADR-0035: job control (disegno Fase 44a)

## Status

Accepted (implementazione in Fase 44a chiusa con questa ADR). Resta alla
Fase 44b: Ctrl-C selettivo (cancel nativo cooperativo + escalation),
causa morte 128+sig. Job multi-pid (`&` su pipeline): fase futura
(spostato da "Fase 44" di ADR-0032, che resta snapshot storico).

## Context

La shell lanciava programmi con job control minimale (Fase 37.2: tabella
job, `wait` bloccante, niente foreground killable). La 44a chiede Ctrl-Z +
`fg`/`bg` reali. Vincoli:

- Kernel neutro (ADR-0025): niente segnali numerati, path, termios nel
  kernel — solo meccanismo (canali, stati, ready queue).
- Stati processo: solo Ready/Blocked/Terminated; `pending_wake` morto;
  nessuna primitiva di wake/sospensione; `ps` a due valori (0/1).
- La shell attende il fg in `recv()` bloccante (`wait_job`) senza leggere
  la tastiera; al ritorno l'editor scarta `0x03`/`0x1a`.
- Identità affidabile dei job = canale di nascita, non pid (riuso PID).
- Ctrl-C/Z arrivano come byte `0x03`/`0x1a` al device tastiera (tty non
  filtra); unico lettore = shell.

## Decision

1. **Meccanismo kernel: `suspended: bool` + `SYS_SUSPEND(50)` /
   `SYS_RESUME(51)`.** Il flag e' ortogonale a `state`/`ipc_state`; unico
   choke point `set_ready` che salta i sospesi (tutti i wake IPC/IRQ/tick
   lasciano i messaggi in coda senza risvegliare). Suspend toglie dalle
   ready solo i Ready (i Blocked non ci sono gia'); resume rientra in Ready
   (o sveglia subito un `BlockedOnRecv` con coda non vuota — `ipc_recv`
   ricontrolla la coda al ritorno dallo switch). Gate parent/init come
   `kill` (Fase 35); mai init/self/kernel/terminati; idempotenti.
   `terminate` azzera il flag (il morto non torna). `ps` = Stopped (2).
   Scartato: variante `State::Stopped` (ripple su tutti i match) e riuso di
   `Blocked` (nessuna via di risveglio esterno: `pending_wake` e' morto).
2. **Semantica in shell.** `JobState { Running, Stopped, Done }`; spec
   `%N` (indice `jobs`) o pid; `fg`/`bg` + `jobs` con stato; `wait` salta
   gli Stopped (report senza attesa). `wait_fg`: durante il fg alterna
   `recv_poll` (drena EXIT, reply difensiva, registra le morti altrui in
   tabella) e tastiera non bloccante, con budget di spin puri tra i giri
   (anti-dilution, lezione Fase 21). Solo Ctrl-Z (0x1a) interessa, solo per
   `run` singolo (pipeline fg = nessun intercept, per decisione); altri
   tasti scartati (documentato). A Ctrl-Z sospende il figlio e annuncia
   `[N]+ Stopped`; race morte/sospensione decisa da un ultimo drain (Exited
   vince) + self-healing via `poll_reap` (Stopped→Done).
3. **Ctrl-C in 44a = niente** (scartato come gli altri tasti; help e docs lo
   dicono: e' Fase 44b). Causa morte 128+sig in 44b.
4. **Test**: in-guest `t55` (gate parent/self/morto, TIME congelato su
   running, coda-senza-sveglia + reply su resume per un bloccato, hardening
   non-parent); `test-shell-44.py` (14 check: bg/fg/Done, 2 bg, Ctrl-Z
   selettivo, `ps` stopped, bg-resume, loop, errori, cleanup). Harness:
   `run_until` (attesa su pattern: fork/input lenti superano gli sleep
   fissi) + `%` in KEYMAP (`shift-5`).

## Consequences

### Positive

- Job control reale senza segnali nel kernel: `ps` Stopped, TIME congelato,
  messaggi in coda mai persi, morti mai mascherate.
- Gate 55/55; shell 174 check (160 + 14).

### Negative

- `wait` bloccante resta senza intercept (Ctrl-Z durante `wait` = niente;
  i tasti si accumulano e compaiono al prompt dopo — comportamento
  pre-esistente, documentato).
- `ps` allarga la colonna STATE a 7 (`stopped`, come `blocked`).
- Polling fg: la shell consuma quanti durante il fg (throttled, accettato).

### Neutral

- `&` su pipeline resta rifiutato (job multi-pid = fase futura).
- Dettaglio implementativo: `note_exit` registra in tabella anche le morti
  altrui viste durante le attese (prima `wait_job` le scartava: un bg morto
  durante un fg restava `run` per sempre).
