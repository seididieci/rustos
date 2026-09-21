# ADR-0025: Modello nativo + POSIX come personalità (identità del sistema)

## Status

Accepted.

## Context

ADR-0015 ha stabilito che POSIX è API di `libr`, non ABI del sistema. Restava
implicito il passo successivo: se POSIX è solo una personalità, qual è il
modello *nativo* per le app dell'OS — e come si evita lo scivolamento verso
"UNIX con un microkernel dentro" a ogni nuova primitiva (il rischio si
concretizza con `exec`: argv/envp/exit-code sono convenzioni POSIX che
potrebbero diventare struttura)? Discusse e scartate: rincorrere interfacce di
modelli-macchina divergenti (stile DOS: niente processi/protezione, hardware
diretto) — non è un'interfaccia da implementare ma un'emulazione da scrivere,
a fronte di software che vuole il metallo; costo/modello inaccettabile
(vedi Consequences).

## Decision

1. **Nucleo unico, personalità userspace** (precedente: Windows NT). Il kernel
   conosce solo meccanismo (canali, frame, ring, PTE, refcount); mai concetti
   di personalità: niente errno, segnali-POSIX, path, termios, utenti.
2. **Modello nativo, principi** (emerge dai meccanismi, non disegnato a priori):
   riferimenti opachi invece di nomi globali (canali, ADR-0008); messaggi
   strutturati invece di byte opachi (frame FS, non `read`/`write`); diritti
   solo in riduzione (Fase 17); async-first (`send_async` + router, ADR-0009/
   0019); `Result` invece di errno globale; creazione esplicita invece di
   eredità implicita (`spawn_image` + canali espliciti; `fork` esiste per POSIX
   coi suoi caveat, ADR-0024, e non si estende il modello fork).
3. **POSIX è traduzione.** Wrapper + convenzioni in `libr` (argv/envp sullo
   stack, exit-code, `-1`); stato globale futuro (job control, segnali) in un
   `posix-server`, mai nel kernel. Livello attuale: subset pragmatico;
   obiettivo: compatibilità spinta se/quando si porta software reale.
4. **Test di revisione per ogni syscall nuova:** "ha senso per entrambe le
   personalità?" Se serve solo a POSIX, vive in `libr`/server.
5. **Trigger di review esplicito:** si rivede questa ADR quando esistono 2+
   app native senza POSIX-ismi (il modello si consolida dall'uso, non dalla
   carta).

## Consequences

### Positive

- Il kernel resta neutro e piccolo: ogni personalità futura riusa lo stesso
  meccanismo senza toccarlo (`SYS_EXEC` serve a entrambe).
- `fork`/`exec`/segnali restano confinati alla personalità POSIX; il nativo
  non eredita i loro compromessi.

### Negative

- Doppio vocabolario in `libr` (nativo + POSIX) da mantenere coerente.
- Una seconda personalità nativa vera costa app dogfood che ancora non ci sono.

### Neutral

- argv/envp in stile Linux dentro `SYS_EXEC` sono convenzioni di dati,
  non identità POSIX: il formato è documentato come interfaccia versionabile
  (stessa regola del protocollo FS, ADR-0015 §2).

## Alternatives Considered

- **Convergenza POSIX strutturale:** scartata — congela il disegno e rende il
  kernel ostaggio dello standard (è quello che ADR-0015 vieta).
- **Personalità DOS/retrocomputing:** scartata — modello-macchina divergente
  (no processi/protezione), richiede emulazione (NTVDM docet, poi abbandonata);
  a parità di sforzo POSIX porta ordini di grandezza più software.
- **Specifica completa del modello nativo ora:** scartata — carta senza
  consumatori; si consolida con l'uso (punto 5).

## References

- ADR-0015 (POSIX come API di `libr`), ADR-0008 (canali), ADR-0009/0019 (async),
  ADR-0024 (`fork` e i suoi caveat), Fase 17 (diritti solo in riduzione)
- Fase 37 (`exec` in-place, prima consumatrice neutra) e Fase "posix-server" (futura)
