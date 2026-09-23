# ADR-0032: pipe + heredoc (disegno Fase 42, P3)

## Status

Accepted (implementazione in Fase 42 chiusa con questa ADR). Chiude il
non-scope pipe/heredoc di ADR-0031; `&` su pipeline resta alla Fase 44.

## Context

La Fase 42 dà alla shell le pipeline (`a | b | ...`), l'heredoc (`<<DELIM`)
e il waitpid di gruppo con status. Vincoli ereditati:

- Kernel neutro (ADR-0025): nessuna syscall nuova per le pipe; solo i ring
  SPSC esistenti (`SYS_RING_ALLOC`, 26) e `fork`/`spawn_image`/`EXIT_NOTIFY`.
- fd server-side chiave `(chan,fd)` in userfs (ADR-0031): gli stadi post-fork
  (chan diverso) non usano i fd del parent — serve un handoff come per i
  redirect (modello B: grant single-use + claim).
- Figlio post-fork con FS avvelenato (`post_fork_child`): il figlio builtin
  deve re-inizializzare l'FS (`fs_child_reinit`: lookup + ring freschi +
  handshake, zero aliasing) prima di usare qualunque fd.
- I lettori/scrittori sono processi distinti con scheduling preemptive: la
  semantica dev'essere non-bloccante lato server (mai `send`/`recv` dentro
  un handler userfs single-threaded) con attesa throttled lato client.

## Decision

1. **Meccanismo in userfs (feature dell'OS), specifica POSIX in
   posix/shell.** La pipe è un `FileEntry::Pipe` con buffer server-side
   (`PipeTable`, cap 8192 byte): come i file, il data plane resta diretto
   client→userfs senza relay. `userland/posix` resta skeleton (P1): la
   semantica `pipe()`/`dup2()` POSIX è API di `libr`/shell, non stato del
   server. Scartato: pipe-buffer nel posix-server (nota di ADR-0031) —
   avrebbe duplicato grant/claim/handoff già provati in userfs per un
   beneficio nullo in P3.
2. **Protocollo**: `R_PIPE_CREATE=0x20` (nessun payload, w0 = hint capacità),
   ritorna due fd (lettura, scrittura) sul canale del chiamante; sentinelle
   `ERR_EMPTY` (pipe vuota/piena con peer vivi: non EOF, riprova throttled)
   ed `ERR_CLOSED` (estremità opposta chiusa: read→EOF, write→errore).
   `libr::pipe()` + `Error::Empty`/`Closed` (EAGAIN/EPIPE al bordo POSIX,
   Mai `-errno` sul wire). t53 esteso a 17 voci.
3. **Handoff stadi = grant con reservation al grant.** Il parent crea la pipe
   e registra un grant per stadio (stesso nonce COW di ADR-0031); la
   reservation conta le estremità al grant — non al claim — così il close
   del parent prima del claim del figlio non libera il buffer (race
   osservata e fixata: dal 2° link l'id pipe è ignoto al claim). Claim senza
   conteggi, cancel/purge rilasciano, close/purge pipe-aware.
4. **Client bloccante sopra server non-bloccante.** `read_fs`/`write_fs`
   riprovano throttled (solo spin puri IF=1, mai `get_ticks` in loop) su
   `Empty` e completano i parziali; i file non emettono mai `Empty`
   (invariati). EOF vero: nessuna estremità di scrittura → read 0.
5. **Shell: `cmd_pipeline` (fork per stadio, grant per stadio, `wait_all`).**
   Status del gruppo = ultimo stadio (bash); `$?` threadato; redirect
   file/heredoc espliciti vincono sui pipe-link per-slot. Heredoc: corpo
   letterale letto pre-exec (prompt secondario `> `) e scritto
   nell'estremità dopo il fork; `&` su pipeline multi-stadio rifiutato con
   messaggio (Fase 44, mai hang); pipe trailing ignorata come gli altri
   connettori.

## Consequences

### Positive

- Zero kernel, zero nuove syscall; `userfs`/`console`/`devfs` invariati nel
  protocollo (solo userfs cresce di `pipes.rs` + rami handler).
- Streaming oltre la capacità (8192): produttore/consumatore in stadi
  distinti si intercalano via scheduler, nessun deadlock dimensionale.
- Stadi `run` (fork+exec con grant, non builtin) e builtin condividono
  `dispatch_builtin` e handoff: `run … | cat`, `cat … | wc` funzionano.

### Negative

- `write` senza lettori = errore immediato (niente SIGPIPE: niente segnali
  in P3, il chiamante vede l'errore e decide).
- `&` su pipeline rifiutato fino alla Fase 44 (job control).

### Neutral

- Cap 8192 fissa (una pagina dati meno header): oltre si fa streaming, mai
  configurabile in P3.
- Offset pipe ignorati (append/offset non hanno senso sulle estremità).

## Bug veri trovati

1. Race reservation: conteggio estremità ai close concorrenti liberava il
   buffer prima del claim → reservation al grant.
2. Lettori trattavano `Empty` come EOF/fatale (hang-by-race) → retry
   bloccante throttled lato `libr`.
3. Check con aspettative sbagliate (`cat` aggiunge sempre `\n` finale:
   pipe = file + 1 linea/byte).

## Non-scope (Fase 44+)

`&` su pipeline, job control interattivo, segnali (SIGPIPE/SIGINT),
dup condiviso oltre l'handoff, capacità configurabile.

## References

- ADR-0031 (handoff/redirect, modello B), ADR-0030 (fondamenta posix),
  ADR-0025 (kernel neutro, POSIX personalità), ADR-0008 (canali),
  ADR-0014 (diritti), roadmap Fasi 39-45 in `AGENTS.md`
