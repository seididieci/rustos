# ADR-0031: fd virtuali + redirect file (disegno Fase 40, P1)

## Status

Accepted (disegno; implementazione in Fasi 40.0-40.5). Decide il modello dup
(opzione B) e l'architettura P1; i dettagli di pipe/job restano alla Fase 42.

## Context

La Fase 40 dà alla shell i redirect (`> >> < 2> 2>&1`) e al sistema gli fd
virtuali 0/1/2, primo gradino posix dopo le fondamenta della Fase 39
(ADR-0030: registry, `Error` nativo, harness). Vincoli ereditati:

- fd server-side chiave `(chan,fd)` in userfs: il figlio post-fork (chan
  diverso) non usa i fd del parent; dopo `exec` i canali restano (stesso PID).
- Figlio post-fork con FS avvelenato (`post_fork_child`); IPC pura sempre
  disponibile; COW condivide la memoria finche' nessuno scrive (precedente:
  la shell passa gia' `img`/`buf` al figlio cosi').
- Kernel neutro (ADR-0025): nessuna syscall nuova; solo `peer_pid`/`ps_info`
  esistenti per l'attestazione.

## Decision

1. **Dup modello B (grant single-use + claim, offset copiato, entry
   indipendente).** Scartate: A (riapertura: TOCTOU + doppio open su disco +
   O_CREAT rieseguito) e C (offset condiviso: stessa performance di B ma tocca
   close/purge/EBUSY/DEV_CLOSE e richiede `dup` driver-side, per un beneficio
   inosservabile in P1 — il parent chiude la sua copia subito dopo il fork).
   Performance: 1 open + 2 IPC (~4µs) vs 2 open (fino a ms su miss FAT);
   steady-state zero in tutti i casi (data plane diretto invariato).
2. **Attestazione senza token server**: nonce 64-bit (`rdtsc ^ pid ^ ticks`)
   via memoria COW; al claim userfs verifica `ps_info(claimant).parent ==
   registrant_pid` **e** `peer_pid(chan_registrant) == registrant_pid`
   (anti riuso-PID). Remote → `Invalid` in P1. Cancel best-effort dal parent
   nel cleanup wait; purge dei grant su EXIT_NOTIFY del registrante.
   `R_DUP_*` sempre consentiti (capability come CLOSE, mai path).
3. **Handoff via COW, nessun protocollo IPC posix in P1.** Il parent scrive
   `spec = [(vfd, grant_nonce)]` pre-fork; il figlio legge, fa claim, riempie
   la tabella stdio `libr` ed `exec`-a. `userland/posix` in P1 e' skeleton
   supervisionato (register/ready/igiene EXIT_NOTIFY/tabelle stub per la 42):
   fondazione + supervisione testata, contenuto dopo.
4. **Print routing in `libr`**: `print` → `write_fs(stdio.out)` quando
   impostato, altrimenti seriale; fallback seriale a write fallita (l'output
   debug non si perde mai). Stdin shell instradato allo stesso modo. Senza,
   `>` non catturerebbe i programmi reali (che usano `println!`).
5. **Costanti**: `R_LSEEK=0x1C` (solo Local; Remote→`Invalid`, dir→`IsDir`),
   `R_DUP_GRANT=0x1D`/`CLAIM=0x1E`/`CANCEL=0x1F`, `SEEK_*`, `O_TRUNC=0x400`,
   `O_APPEND=0x800` (flag `append` su `FileEntry::Local`, seek-to-end per
   write), `RIGHTS_SEEK=0x100` con `ALL`→0x1FF (solo memoria, effimeri),
   sentinelle `ERR_*` alte (`!0-2…`, mai `-errno`: `-2` colliderebbe con
   `ERR_NOHANDSHAKE`). `2>&1` = alias vfd (zero userfs).

## Consequences

### Positive

- `ftable` invariata nei percorsi close/purge/EBUSY (ogni entry resta fd
  normale); diritti Fase 17 intoccati (fd = capability pure, check al grant
  mai su path — non ce n'e').
- Primo codice di dominio osservabile (`NotFound` all'open) senza toccare il
  kernel; `fs_reply_check` diventa tabella codice→`Error`.

### Negative

- Offset "dup" copiato, non condiviso POSIX: documentato come
  dup-for-handoff; il dup condiviso vero (se mai servira') e' una nuova op.
- `userland/posix` P1 quasi vuoto: voluto (skeleton + supervisione testata),
  il contenuto e' Fase 42+.

### Neutral

- Shell con mini-lexer redirect separato dal parser Fase 41; builtin con
  stdio temporaneo + restore; `cmd > f &` funziona (spec + bg ortogonali).

## Non-scope (Fase 42+)

Pipe `|`, heredoc, env/PATH, `O_EXCL`/`O_RDWR`, dup/seek remoti, seek su dir,
tabelle posix usate, piu' di 3 vfd.

## References

- ADR-0030 (fondamenta), ADR-0025/0015 (kernel neutro, POSIX in `libr`),
  ADR-0008 (canali), ADR-0014 (diritti), roadmap Fasi 39-45 in `AGENTS.md`
