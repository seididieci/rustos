# ADR-0033: env/PATH/script (disegno Fase 43a, P4)

## Status

Accepted (implementazione in Fase 43a chiusa con questa ADR). Resta alla
Fase 43b: history/editing di linea (tocca `usertty`, frecce oggi scartate).

## Context

La Fase 43 prima parte (43a) dà alla shell ambiente reale, ricerca PATH ed
eseguibili script. Vincoli ereditati:

- Kernel neutro (ADR-0025/0026): argv/envp sullo stack sono convenzione di
  dati versionabile, mai struttura; il kernel non tocca il FS (ADR-0005) e
  non interpreta i byte (niente `NAME=val`, PATH, `#!`, cwd nel kernel).
- Blocco `SYS_EXEC` esistente `[argc:8][payload]` con magic redirect come
  ultimo argv (40.4c): l'estensione non deve spostarlo né romperne il
  riconoscimento (`has_redir_magic` sull'ultimo argv).
- `VAR=v cmd` rifiutato dal parser (`EnvPrefix`, Fase 41); `run` a path
  esatti (ADR-0028); VARS/CWD solo client-side shell.
- Self-hosting come discriminante del modello env: un futuro toolchain ha
  bisogno che le variabili raggiungano davvero i figli; il flag export stile
  bash non aggiunge nulla a quello scopo.

## Decision

1. **Formato blocco `[argc:8][envc:8][argv...][magic?][env...]`** (ordine
   argv/magic/env è il contratto: il kernel legge argc stringhe poi envc).
   Budget unico `ARGS_MAX` 8 KiB (single source `syscall-numbers`); bound
   argc/envc ≤ 1024 l'uno; fit stack pre-verificato, validazione prima di
   toccare stato come in 37.1. Kernel opaco: mai ispezione `=`.
   `setup_user_stack` invariato (argc=0/envc=0 = le stesse 3 parole).
2. **Tutte le VARS shell → envp** (niente flag export): `child_env` =
   prefissi (ultimo vince) + VARS + `PWD=cwd` se assente. `Env::get` vede la
   prima occorrenza (l'ordine è la priorità). `libr::{Env, env_from_stack,
   serialize_argv_redir_env, exec_env}`; `runhello` dumpa l'env (serve ai
   test). Scartato: solo-exportate (stato parser in più, zero benefici
   self-hosting).
3. **`VAR=v cmd` per ogni comando**: builtin/source in-processo con
   save/set/restore (`vars_unset` nuovo); esterni via blocco envp; in
   pipeline builtin nel figlio (scoped), esterni nel blocco di stadio.
   `$VAR` nella stessa riga vede il vecchio (espansione al parse, come bash).
4. **PATH + bare word + shebang in shell** (mai nel kernel): `resolve_prog`
   (`/` diretto, else `$PATH`, default `/fat/bin`, sonda `stat`, fallback
   `.bin` per FAT 8.3); bare word non-builtin = run implicito (ignoto 127,
   `argv[0]` = path risolto); shebang `#!interp [arg]` sniffato dopo
   `load_file` con `argv=[interp, script, args...]`, ricorsione bound 4.
5. **In-guest**: gamba argv di t52 estesa con `T52E=envok` (stesso `T_DONE`,
   nessun protocollo nuovo); shell coperta da `test-shell-43.py` (17 check).

## Consequences

### Positive

- `export FOO=bar` + programma vede `FOO=bar`; `PWD` sempre presente;
  `runhello` senza path; `./x.sh` eseguibile. Base per toolchain futuro.
- Kernel verificabilmente neutro: `rg "environ|getenv|PATH|shebang|chdir"
  kernel/` deve restare vuoto fuori dai commenti "opaco".

### Negative

- `argv[0]` delle bare word = path risolto, non digitato (deviazione
  documentata); ` VAR=v ` con redirect esterno+interni annidati resta
  limitato (restore interno cancella l'esterno, limite `source` noto).
- Doppio messaggio (`cannot load` + `unknown command`) per stadi pipe
  ignoti senza `/` (loud, mai silenzioso).

### Neutral

- Bug vero trovato: magic dopo gli env invece che ultimo argv (incrocio
  argv/env solo con redirect+env insieme) — il contratto d'ordine sopra lo
  fissa; 42 (magic senza env) e p43b (env senza magic) non lo vedevano.
