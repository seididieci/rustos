# ADR-0030: Fondamenta posix — registry, errore nativo, harness (Fase 39)

## Status

Accepted (Fase 39 implementata; le fasi 40+ avranno ADR propri).

## Context

La roadmap Fasi 39-45 (in `AGENTS.md`) porta una personalita' POSIX in
userspace — `posix-server` + shell avanzata — verso il self-hosting, col
kernel sempre neutro (ADR-0025) e POSIX come sola API di `libr` (ADR-0015).
La Fase 39 e' il P0: tre agganci senza i quali le fasi successive non possono
esistere (uno slot nome per il futuro server, un tipo errore condiviso, un
test che li fissa), e niente altro — nessun server, nessun cambio di percorso
dati, comportamento identico piu' un enum.

## Decision

1. **Registry 8→16 + `Service::Posix = 8`.** Discriminant 0-7 storici intoccati
   (ABI stabile); slot 9-15 liberi per futuri servizi senza ritoccare il
   kernel. Unica modifica kernel: braccio nome in `syscall/service.rs`
   (il gate init-child e la tabella `SERVICE_OWNER` seguono da soli).
   Fix collaterale obbligato: `service_from_disc` usava `transmute` su
   `disc < SERVICE_COUNT` — con slot liberi 9-15 il transmute di un discriminant
   senza variante sarebbe UB; ora match esplicito (solo gli slot assegnati
   risolvono).
2. **Errore nativo `libr::posix::Error` + UNICA `to_errno` al bordo POSIX.**
   L'enum e' del dominio OS (varianti di trasporto osservabili in Fase 39:
   `NotReady/Pending/RingFull/ServerDied/Denied/NoMemory/Busy/Invalid/Failed`;
   varianti di dominio `NotFound/.../TooBig` dichiarate con mapping fissato ma
   senza produttori fino alla Fase 40). I numeri errno POSIX vivono SOLO in
   `to_errno` (match totale: una variante senza braccio non compila) — mai nel
   kernel ne' sul wire. Il caso peggiore di un bug li' e' un numero sbagliato
   in un messaggio: le decisioni usano le varianti, non i numeri.
   `WaitReplyError` resta (gia' tipato, con payload pid/code per chi li serve).
3. **Migrazione TOTALE dei wrapper a `Result<T, Error>` in un colpo solo**
   (vecchi nomi; `read_fs`/`write_fs` ritornano `usize` con parziale-come-`Ok`
   alla POSIX; `open_wait`/`readdir`/`rights_get` tipati; anche ipc/spawn/sys).
   Fix meccanico dei chiamanti guidato dal compilatore, gate verde come
   arbitro. Restano fuori per disegno: `write` seriale (debug facility, il
   fallimento e' insignificante), `ps_info/text_stats` (`Option`/tuple),
   `poll_wait` (generici).
4. **Harness t53** (nuovo modulo `t_posix.rs`, futura casa dei test posix):
   lookup/pid di Posix pre-server = `NotFound` pulito; tabella `to_errno`
   totale; gate di registrazione sul nuovo slot via helper HARDEN esteso
   (non-figlio-di-init: kill + register Init + register Posix rifiutati).
   t50 intatto.

## Consequences

### Positive

- Le Fasi 40+ hanno l'aggancio nome (`Posix = 8`) e il tipo errore gia' in uso
  da tutti i chiamanti: niente flag-day a meta' strada.
- La migrazione in un colpo solo evita anni di doppie API (`_ex` accanto alle
  vecchie): il compilatore ha elencato ogni sito.

### Negative

- Diff largo (~30 file) per una fase "fondamenta": il rischio era rottura
  silenziosa di semantica (es. parziali di read/write). Mitigato: semantica
  parziale-come-`Ok` documentata nei wrapper + suite invariata come prova.

### Neutral

- `SPAWN_IMAGE_MAX` promossa a single source in `syscall-numbers` (il kernel
  la riusa; `libr` pre-valida per errori precisi): pulizia abilitata dalla
  fase, non richiesta da essa.

## Limiti dichiarati (Fase 40)

- userfs risponde ancora solo `ERR` generico → tutto collassa in `Failed`;
  i codici distinti (`NotFound` vs `EROFS` osservabile) richiedono modifiche
  userfs.
- Niente `unregister`: uno slot occupato resta occupato fino alla morte
  dell'owner (come prima).
- `ServerDied` senza payload (chi serve pid/code usa `WaitReplyError`).

## References

- ADR-0025 (kernel neutro), ADR-0015 (POSIX in `libr`), ADR-0008 (registry),
  ADR-0014 (diritti, self-restriction), roadmap Fasi 39-45 in `AGENTS.md`
