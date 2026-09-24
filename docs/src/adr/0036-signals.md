# ADR-0036: segnali nativi cooperativi (disegno Fase 44b)

## Status

Accepted (implementazione in Fase 44b chiusa con questa ADR). Chiude la
Fase 44 (44a job control in ADR-0035). Job multi-pid (`&` su pipeline) e
posix-server restano futuri.

## Context

La 44a ha dato suspend/resume + fg/bg + Ctrl-Z senza toccare i segnali
(Ctrl-C scartato). La 44b chiede Ctrl-C selettivo "catturabile" e causa di
morte. Vincoli:

- Kernel neutro (ADR-0025): niente segnali numerati nel kernel — la consegna
  asincrona con handler (POSIX) resta al posix-server futuro.
- Precedente 128+sig: `FAULT_EXIT_CODE` = 139 = 128+11 (Fase 29).
- Il parent ha il canale di nascita di ogni job (identità affidabile);
  `send_async` non blocca mai; `kill` parent-scoped con cascata (Fase 14).
- Ctrl-C arriva come byte 0x03 al device tastiera; `wait_fg` (44a) già
  alterna recv_poll + tastiera non bloccante.

## Decision

1. **Cancel cooperativo su messaggio, non segnale kernel.** Tag
   `JOB_CANCEL` (0x43, w0 = 2/SIGINT informativo) sul canale di nascita via
   `send_async`: il programma bloccato in `recv` si sveglia e gestisce
   (cleanup + exit a sua scelta); un CPU-bound non lo vede mai (come POSIX:
   senza handler non c'è catch). Niente consegna forzata, niente maschere,
   niente `sigreturn` — quello e' posix-server.
2. **Escalation con causa.** Se dopo un grace di ~20 tick (throttled, igiene
   scheduler) il job e' vivo: `kill(EXIT_SIGINT)` con 130 = 128+2
   (`EXIT_SIGINT` in `syscall-numbers`, stessa convenzione di
   `FAULT_EXIT_CODE`; solo convenzione al bordo, il kernel vede un code).
   La morte e' sempre osservata via EXIT_NOTIFY (mai presunta); Exited vince
   sul grace come su Ctrl-Z.
3. **Shell**: 0x03 in `wait_fg` → `cancel_job` (drain continuo: reply altrui,
   tastiera scartata senza re-trigger) → `Exited(code)`; il percorso fg
   stampa `[exit 130]` come qualunque code != 0. Solo fg singolo (come
   Ctrl-Z); bg intatto (selettività); `wait` bloccante invariato.
4. **Test**: in-guest `t56` (catcher esce 42 al cancel senza kill; KILLME
   resta vivo oltre il grace poi esce 130 via kill); `test-shell-44b.py`
   (5 check: Ctrl-C end-to-end → 130 su uptime, selettività, cleanup).

## Consequences

### Positive

- Ctrl-C selettivo funzionante + catch dimostrabile, zero syscall nuove,
  zero codice kernel (solo tag + const: neutralità verificabile con `rg`).
- Causa di morte uniforme 128+sig al bordo (139 fault, 130 SIGINT).

### Negative

- Senza handler POSIX: un programma che vuole pulire DEVE leggere il canale
  di nascita (cooperativo esplicito, documentato). Chi non lo fa muore 130.
- Grace fissa ~20 tick (euristica, non garantita: un cooperante lentissimo
  viene killato — documentato come limite del modello nativo).

### Neutral

- `&` su pipeline: ancora rifiutato (job multi-pid, fase futura).
- Il secondo Ctrl-C durante il grace non riavvia il grace (scartato).
